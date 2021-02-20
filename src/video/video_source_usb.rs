use super::types::{FrameSize, VideoEncodeType, VideoSourceType};
use super::video_source::VideoSource;
use regex::Regex;
use serde::{Deserialize, Serialize};
use v4l::prelude::*;
use v4l::video::Capture;

use super::types::*;
use super::xml::*;
use log::*;

#[derive(Clone, Debug, Serialize)]
pub struct UsbBus {
    pub domain: u8,
    pub bus: u8,
    pub device: u8,
    pub first_function: u8,
    pub last_function: u8,
}

#[derive(Clone, Debug, Serialize)]
pub struct VideoSourceUsb {
    pub name: String,
    pub device_path: String,
    pub usb_bus: UsbBus,
    controls: Vec<Control>,
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

impl VideoSourceUsb {
    //TODO: move to video source
    fn control_value(&self, id: u32) -> std::io::Result<i64> {
        let device = Device::with_path(&self.device_path)?;
        let value = device.control(id)?;
        match value {
            v4l::control::Control::String(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "String control type is not supported.",
                ));
            }
            v4l::control::Control::Value(value) => return Ok(value as i64),
            v4l::control::Control::Value64(value) => return Ok(value),
        }
    }
}

fn convert_v4l_framesize(frame_sizes: &Vec<v4l::FrameSize>) -> Vec<FrameSize> {
    let frame_sizes: Vec<FrameSize> = frame_sizes
        .iter()
        .map(|frame_size| match &frame_size.size {
            //TODO: Move to map_while after release
            v4l::framesize::FrameSizeEnum::Discrete(discrete) => Some(FrameSize {
                encode: VideoEncodeType::from_str(frame_size.fourcc.str().unwrap()),
                height: discrete.height,
                width: discrete.width,
            }),
            v4l::framesize::FrameSizeEnum::Stepwise(stepwise) => {
                warn!("Ignoring stepwise source: {:#?}", frame_size);
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
            frame_sizes.append(&mut convert_v4l_framesize(
                &device.enum_framesizes(format.fourcc).unwrap(),
            ));
        }

        return frame_sizes;
    }

    fn configure_by_name(&self, _config_name: &str, _value: i64) -> std::io::Result<()> {
        unimplemented!();
    }

    fn configure_by_id(&self, config_id: u64, value: i64) -> std::io::Result<()> {
        let control = self
            .controls()
            .into_iter()
            .find(|control| control.id == config_id);

        if control.is_none() {
            let ids: Vec<u64> = self.controls().iter().map(|control| control.id).collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Control ID '{}' is not valid, options are: {:?}",
                    config_id, ids
                ),
            ));
        }

        //TODO: Add control validation
        let device = Device::with_path(&self.device_path).unwrap();
        //TODO: we should handle value, value64 and string
        return device.set_control(config_id as u32, v4l::control::Control::Value(value as i32));
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
                error!(
                    "Failed to capture caps for device: {} {:#?}",
                    camera_path, error
                );
                continue;
            }
            let caps = caps.unwrap();

            if let Err(error) = camera.format() {
                error!(
                    "Failed to capture formats for device: {}\nError: {:#?}",
                    camera_path, error
                );
                continue;
            }

            cameras.push(VideoSourceType::Usb(VideoSourceUsb {
                name: caps.card,
                device_path: camera_path.clone(),
                usb_bus: UsbBus::from_str(&caps.bus).unwrap(),
                controls: Default::default(),
            }));
        }

        return cameras;
    }

    fn controls(&self) -> Vec<Control> {
        if !self.controls.is_empty() {
            return self.controls.clone();
        }

        //TODO: create function to encapsulate device
        let device = Device::with_path(&self.device_path).unwrap();
        let v4l_controls = device.query_controls().unwrap_or_default();

        let mut controls: Vec<Control> = vec![];
        for v4l_control in v4l_controls {
            let mut control: Control = Default::default();
            control.name = v4l_control.name;
            control.id = v4l_control.id as u64;
            let value = self.control_value(v4l_control.id);
            if let Err(error) = value {
                error!(
                    "Failed to get control '{} ({})' from device {}: {:#?}",
                    control.name, control.id, self.device_path, error
                );
                continue;
            }
            let value = value.unwrap();
            let default = v4l_control.default;

            match v4l_control.typ {
                v4l::control::Type::Boolean => {
                    control.cpp_type = "bool".to_string();
                    control.configuration = ControlType::Bool(ControlBool { default, value });
                    controls.push(control);
                }
                v4l::control::Type::Integer | v4l::control::Type::Integer64 => {
                    control.cpp_type = "int64".to_string();
                    control.configuration = ControlType::Slider(ControlSlider {
                        default,
                        value,
                        step: v4l_control.step,
                        max: v4l_control.maximum,
                        min: v4l_control.minimum,
                    });
                    controls.push(control);
                }
                v4l::control::Type::Menu | v4l::control::Type::IntegerMenu => {
                    control.cpp_type = "int32".to_string();
                    if let Some(items) = v4l_control.items {
                        let options = items
                            .iter()
                            .map(|(value, name)| ControlOption {
                                name: match name {
                                    v4l::control::MenuItem::Name(name) => name.clone(),
                                    v4l::control::MenuItem::Value(name) => name.to_string(),
                                },
                                value: *value as i64,
                            })
                            .collect();
                        control.configuration = ControlType::Menu(ControlMenu {
                            default,
                            value,
                            options,
                        });
                        controls.push(control);
                    }
                }
                _ => continue,
            };
        }
        return controls;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_test() {
        for camera in VideoSourceUsb::cameras_available() {
            if let VideoSourceType::Usb(camera) = camera {
                println!("{:#?}", camera);
            }
        }
    }
}
