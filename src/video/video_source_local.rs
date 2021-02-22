use super::types::{FrameSize, VideoEncodeType, VideoSourceType};
use super::video_source::VideoSource;
use regex::Regex;
use serde::Serialize;
use v4l::prelude::*;
use v4l::video::Capture;

use super::types::*;
use log::*;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsbBus {
    pub interface: String,
    pub usb_hub: u8,
    pub usb_port: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum VideoSourceLocalType {
    Unknown(String),
    Usb(UsbBus),
    Isp(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct VideoSourceLocal {
    pub name: String,
    pub device_path: String,
    pub typ: VideoSourceLocalType,
}

impl VideoSourceLocalType {
    // For PCI:
    // https://wiki.xenproject.org/wiki/Bus:Device.Function_(BDF)_Notation
    // description should follow: <domain>:<bus>:<device>.<first_function>-<last_function>
    // E.g: usb-0000:08:00.3-1, where domain, bus and device are hexadecimal
    // `first_function` describes the usb HUB and `last_function` describes the USB port of that HUB
    //
    // For devices that does not have PCI, the information will come with
    // the following description: usb-<unknown>z.<usb-usb_hub>.<usb_port>
    // E.g: usb-3f980000.usb-1.4, where unknown is hexadecimal
    // `udevadm info` can also provide information about the camera
    pub fn from_str(description: &str) -> Self {
        if let Some(result) = VideoSourceLocalType::usb_from_str(description) {
            return result;
        }

        if let Some(result) = VideoSourceLocalType::isp_from_str(description) {
            return result;
        }

        warn!(
            "Unable to identify the local camera connection type, please report the problem: {}",
            description
        );
        return VideoSourceLocalType::Unknown(description.into());
    }

    fn usb_from_str(description: &str) -> Option<Self> {
        let pci_regex = Regex::new(
            r"(?P<interface>[0-9|a-f]+:[0-9|a-f]+:[0-9|a-f]+).(?P<usb_hub>\d+)-(?P<usb_port>\d+)",
        )
        .unwrap();
        let usb_regex =
            Regex::new(r"(?P<interface>[0-9|a-f]+).usb-(?P<usb_hub>\d+).(?P<usb_port>\d+)")
                .unwrap();

        let capture = (|| {
            if let Some(capture) = pci_regex.captures(description) {
                return Some(capture);
            };

            if let Some(capture) = usb_regex.captures(description) {
                return Some(capture);
            };

            debug!(
                "Description does not match both PCI and USB formats: {}",
                description
            );

            return None;
        })();

        let capture = match capture {
            Some(capture) => capture,
            _ => return None,
        };

        let interface = capture.name("interface").unwrap().as_str().parse().unwrap();
        let usb_hub = capture.name("usb_hub").unwrap().as_str().parse().unwrap();
        let usb_port = capture.name("usb_port").unwrap().as_str().parse().unwrap();

        return Some(VideoSourceLocalType::Usb(UsbBus {
            interface,
            usb_hub,
            usb_port,
        }));
    }

    fn isp_from_str(description: &str) -> Option<Self> {
        let regex = Regex::new(r"platform:(?P<device>\S+)-isp").unwrap();
        if regex.is_match(description) {
            return Some(VideoSourceLocalType::Isp(description.into()));
        }
        return None;
    }
}

fn convert_v4l_framesize(frame_sizes: &[v4l::FrameSize]) -> Vec<FrameSize> {
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
                warn!(
                    "Ignoring stepwise '{:#?}', frame_size: {:#?}",
                    stepwise, frame_size
                );
                None //TODO this can be done with frame_size.size.to_discrete()
            }
        })
        .take_while(|x| x.is_some())
        .map(|x| x.unwrap())
        .collect();
    return frame_sizes;
}

impl VideoSource for VideoSourceLocal {
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

        debug!("Checking resolutions for camera: {}", &self.device_path);
        for format in formats {
            frame_sizes.append(&mut convert_v4l_framesize(
                &device.enum_framesizes(format.fourcc).unwrap(),
            ));
        }

        return frame_sizes;
    }

    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        unimplemented!();
    }

    fn set_control_by_id(&self, control_id: u64, value: i64) -> std::io::Result<()> {
        let control = self
            .controls()
            .into_iter()
            .find(|control| control.id == control_id);

        if control.is_none() {
            let ids: Vec<u64> = self.controls().iter().map(|control| control.id).collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Control ID '{}' is not valid, options are: {:?}",
                    control_id, ids
                ),
            ));
        }

        //TODO: Add control validation
        let device = Device::with_path(&self.device_path).unwrap();
        //TODO: we should handle value, value64 and string
        return device.set_control(
            control_id as u32,
            v4l::control::Control::Value(value as i32),
        );
    }

    fn control_value_by_name(&self, control_name: &str) -> std::io::Result<i64> {
        unimplemented!();
    }

    fn control_value_by_id(&self, control_id: u64) -> std::io::Result<i64> {
        let device = Device::with_path(&self.device_path)?;
        let value = device.control(control_id as u32)?;
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

    fn cameras_available() -> Vec<VideoSourceType> {
        let cameras_path: Vec<String> = std::fs::read_dir("/dev/")
            .unwrap()
            .map(|f| String::from(f.unwrap().path().to_str().unwrap()))
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

            let source = VideoSourceLocal {
                name: caps.card,
                device_path: camera_path.clone(),
                typ: VideoSourceLocalType::from_str(&caps.bus),
            };
            cameras.push(VideoSourceType::Local(source));
        }

        return cameras;
    }

    fn controls(&self) -> Vec<Control> {
        //TODO: create function to encapsulate device
        let device = Device::with_path(&self.device_path).unwrap();
        let v4l_controls = device.query_controls().unwrap_or_default();

        let mut controls: Vec<Control> = vec![];
        for v4l_control in v4l_controls {
            let mut control: Control = Default::default();
            control.name = v4l_control.name;
            control.id = v4l_control.id as u64;
            let value = self.control_value_by_id(v4l_control.id as u64);
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

    fn bus_decode() {
        let descriptions = vec![
            (
                // Normal desktop
                VideoSourceLocalType::Usb(UsbBus {
                    interface: "0000:08:00".into(),
                    usb_hub: 3,
                    usb_port: 1,
                }),
                "usb-0000:08:00.3-1",
            ),
            (
                // Provided by the raspberry pi with a USB camera
                VideoSourceLocalType::Usb(UsbBus {
                    interface: "3f980000".into(),
                    usb_hub: 1,
                    usb_port: 4,
                }),
                "usb-3f980000.usb-1.4",
            ),
            (
                // Provided by the raspberry pi with a ISP device
                VideoSourceLocalType::Isp("platform:bcm2835-isp".into()),
                "platform:bcm2835-isp",
            ),
            (
                // Sanity test
                VideoSourceLocalType::Unknown("potato".into()),
                "potato",
            ),
        ];

        for description in descriptions {
            assert_eq!(description.0, VideoSourceLocalType::from_str(description.1));
        }
    }

    fn simple_test() {
        for camera in VideoSourceLocal::cameras_available() {
            if let VideoSourceType::Local(camera) = camera {
                println!("Camera: {:#?}", camera);
                println!("Resolutions: {:#?}", camera.resolutions());
                println!("Controls: {:#?}", camera.controls());
            }
        }
    }
}
