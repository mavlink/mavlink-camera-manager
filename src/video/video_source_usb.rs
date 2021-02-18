use super::video_source::{FrameSize, VideoEncodeType, VideoSource, VideoSourceType};
use regex::Regex;
use v4l::prelude::*;
use v4l::video::Capture;
use serde::{Deserialize, Serialize};

use super::xml::*;

#[derive(Debug, Serialize)]
pub struct UsbBus {
    pub domain: u8,
    pub bus: u8,
    pub device: u8,
    pub first_function: u8,
    pub last_function: u8,
}

#[derive(Debug, Serialize)]
pub struct VideoSourceUsb {
    pub name: String,
    pub device_path: String,
    pub usb_bus: UsbBus,
}

impl UsbBus {
    // https://wiki.xenproject.org/wiki/Bus:Device.Function_(BDF)_Notation
    // description should follow: <domain>:<bus>:<device>.<first_function>-<last_function>
    pub fn from_str(description: &str) -> std::io::Result<Self> {
        let regex = Regex::new(
            r"(?P<domain>\d+):(?P<bus>\d+):(?P<device>\d+).(?P<first_function>\d+)-(?P<last_function>\d+)",
        )
        .unwrap();
        if !regex.is_match(description) {
            panic!("Description is not valid: {:#?}", description);
        }

        let capture = regex.captures(description).unwrap();
        let domain = capture.name("domain").unwrap().as_str().parse().unwrap();
        let bus = capture.name("bus").unwrap().as_str().parse().unwrap();
        let device = capture.name("device").unwrap().as_str().parse().unwrap();
        let first_function = capture
            .name("first_function")
            .unwrap()
            .as_str()
            .parse()
            .unwrap();
        let last_function = capture
            .name("last_function")
            .unwrap()
            .as_str()
            .parse()
            .unwrap();

        return Ok(Self {
            domain,
            bus,
            device,
            first_function,
            last_function,
        });
    }
}

fn convert_v4l_framesize(frame_sizes: &Vec<v4l::FrameSize>) -> Vec<FrameSize> {
    let frame_sizes: Vec<FrameSize> = frame_sizes
        .iter()
        .map(|frame_size| match &frame_size.size { //TODO: Move to map_while after release
            v4l::framesize::FrameSizeEnum::Discrete(discrete) => Some(FrameSize {
                encode: VideoEncodeType::from_str(frame_size.fourcc.str().unwrap()),
                height: discrete.height,
                width: discrete.width,
            }),
            v4l::framesize::FrameSizeEnum::Stepwise(stepwise) => {
                eprintln!("Ignoring stepwise source: {:#?}", frame_size);
                None //TODO this can be done with frame_size.size.to_discrete()
            }
        })
        .take_while(|x| x.is_some())
        .map(|x| x.unwrap())
        .collect();
    return frame_sizes;
}

impl VideoSource for VideoSourceUsb {
    fn name(&self) -> &String {
        return &self.name;
    }

    fn source_string(&self) -> &String {
        return &self.device_path;
    }

    fn resolutions(&self) -> Vec<FrameSize> {
        let device = Device::with_path(&self.device_path).unwrap();
        let formats = device.enum_formats().unwrap_or_default();
        let mut frame_sizes = vec![];
        for format in formats {
            frame_sizes.append(&mut convert_v4l_framesize(&device.enum_framesizes(format.fourcc).unwrap()));
        }

        return frame_sizes;
    }

    fn configure_by_name(&self, _config_name: &str, _value: u32) -> bool {
        unimplemented!();
    }

    fn configure_by_id(&self, _config_id: u32, _value: u32) -> bool {
        unimplemented!();
    }

    fn cameras_available() -> Vec<VideoSourceType> {
        let cameras_path: Vec<String> = std::fs::read_dir("/dev/")
            .unwrap()
            .map(|f| String::from(f.unwrap().path().clone().to_str().unwrap()))
            .filter(|f| f.starts_with("/dev/video"))
            .collect();

        let mut cameras: Vec<VideoSourceType> = vec![];
        for camera_path in &cameras_path {
            let camera = Device::with_path(camera_path).unwrap();
            let caps = camera.query_caps();

            if let Err(error) = caps {
                eprintln!("Failed to capture caps for device: {} {:#?}", camera_path, error);
                continue;
            }
            let caps = caps.unwrap();

            if let Err(error) = camera.format() {
                eprintln!("Failed to capture formats for device: {} {:#?}", camera_path, error);
                continue;
            }

            cameras.push(VideoSourceType::Usb(VideoSourceUsb {
                name: caps.card,
                device_path: camera_path.clone(),
                usb_bus: UsbBus::from_str(&caps.bus).unwrap(),
            }));
        }

        return cameras;
    }

    fn parameters(&self) -> Vec<ParameterType> {
        let device = Device::with_path(&self.device_path).unwrap();
        let controls = device.query_controls().unwrap_or_default();

        let mut parameters: Vec<ParameterType> = vec![];
        for control in controls {
            let name = control.id.to_string();
            let description = Description::new(&control.name);
            let default = control.default;
            let v4l2_id = control.id;
            match control.typ {
                v4l::control::Type::Integer | v4l::control::Type::Integer64 => {
                    let cpp_type = "int64".to_string();
                    parameters.push(ParameterType::Slider(ParameterSlider {
                        name,
                        cpp_type: cpp_type.into(),
                        default,
                        v4l2_id,
                        description,
                        min: control.minimum,
                        max: control.maximum,
                        step: control.step,
                    }));
                }
                v4l::control::Type::Boolean => {
                    let cpp_type = "bool".to_string();
                    parameters.push(ParameterType::Bool(ParameterBool {
                        name,
                        cpp_type: cpp_type.into(),
                        default,
                        v4l2_id,
                        description,
                    }));
                }
                v4l::control::Type::Menu | v4l::control::Type::IntegerMenu => {
                    let cpp_type = "int32".to_string();
                    if let Some(items) = control.items {
                        let options = items
                            .iter()
                            .map(|(value, name)| Option {
                                name: match name {
                                    v4l::control::MenuItem::Name(name) => name.clone(),
                                    v4l::control::MenuItem::Value(name) => name.to_string(),
                                },
                                value: *value,
                            })
                            .collect();

                        parameters.push(ParameterType::Menu(ParameterMenu {
                            name,
                            cpp_type: cpp_type.into(),
                            default,
                            v4l2_id,
                            description,
                            options: Options { option: options },
                        }));
                    }
                }
                _ => continue,
            };
        };
        return parameters;
    }

    fn xml(&self) -> String {
        let definition = Definition {
            version: 1,
            model: Model {
                body: self.name.clone(),
            },
            vendor: Vendor {
                body: "Missing".into(),
            },
        };

        let parameters = self.parameters();
        println!("{:#?}", parameters);

        let mavlink_camera = MavlinkCamera {
            definition,
            parameters: Parameters {
                parameter: parameters,
            },
        };

        use quick_xml::se::to_string;
        return to_string(&mavlink_camera).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_test() {
        for camera in VideoSourceUsb::cameras_available() {
            if let VideoSourceType::Usb(camera) = camera {
                println!("{}", camera.xml());
            }
        }
    }
}
