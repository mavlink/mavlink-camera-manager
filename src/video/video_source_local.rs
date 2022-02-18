use super::types::*;
use super::{
    video_source,
    video_source::{VideoSource, VideoSourceAvailable},
};
use paperclip::actix::Apiv2Schema;
use regex::Regex;
use serde::{Deserialize, Serialize};
use v4l::prelude::*;
use v4l::video::Capture;

use log::*;

//TODO: Move to types
#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum VideoSourceLocalType {
    Unknown(String),
    Usb(String),
    Isp(String),
}

#[derive(Apiv2Schema, Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VideoSourceLocal {
    pub name: String,
    pub device_path: String,
    #[serde(rename = "type")]
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
    //
    // https://www.kernel.org/doc/html/v4.9/media/uapi/v4l/vidioc-querycap.html#:~:text=__u8-,bus_info,-%5B32%5D

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
        let regex =
            Regex::new(r"usb-(?P<interface>(([0-9a-fA-F]{2}){1,2}:?){4})?\.(usb-)?(?P<device>.*)")
                .unwrap();
        if regex.is_match(description) {
            return Some(VideoSourceLocalType::Usb(description.into()));
        }
        return None;
    }

    fn isp_from_str(description: &str) -> Option<Self> {
        let regex = Regex::new(r"platform:(?P<device>\S+)-isp").unwrap();
        if regex.is_match(description) {
            return Some(VideoSourceLocalType::Isp(description.into()));
        }
        return None;
    }
}

impl VideoSourceLocal {
    pub fn update_device(&mut self) -> bool {
        if let VideoSourceLocalType::Usb(our_usb_bus) = &self.typ {
            let cameras = video_source::cameras_available();
            let camera: Option<VideoSourceType> = cameras
                .into_iter()
                .filter(|camera| match camera {
                    VideoSourceType::Local(camera) => match &camera.typ {
                        VideoSourceLocalType::Usb(usb_bus) => *usb_bus == *our_usb_bus,
                        _ => false,
                    },
                    _ => false,
                })
                .next();

            match camera {
                None => {
                    error!("Failed to find camera: {:#?}", self);
                    error!("Camera will be set as invalid.");
                    self.device_path = "".into();
                    return false;
                }
                Some(camera) => {
                    if let VideoSourceType::Local(camera) = camera {
                        if camera.device_path == self.device_path {
                            return true;
                        }

                        info!("Camera path changed.");
                        info!("Previous camera location: {:#?}", self);
                        info!("New camera location: {:#?}", camera);
                        *self = camera.clone();
                        return true;
                    }
                    unreachable!();
                }
            }
        }
        return true;
    }
}

fn convert_v4l_intervals(v4l_intervals: &[v4l::FrameInterval]) -> Vec<FrameInterval> {
    let intervals: Vec<FrameInterval> = v4l_intervals
        .iter()
        .map(|v4l_interval| match &v4l_interval.interval {
            v4l::frameinterval::FrameIntervalEnum::Discrete(fraction) => FrameInterval {
                numerator: fraction.numerator,
                denominator: fraction.denominator,
            },
            v4l::frameinterval::FrameIntervalEnum::Stepwise(_stepwise) => {
                warn!(
                    "Unsupported stepwise frame interval: {:#?}",
                    v4l_interval.interval
                );
                FrameInterval {
                    numerator: 0,
                    denominator: 0,
                }
            }
        })
        .collect();
    return intervals;
}

impl VideoSource for VideoSourceLocal {
    fn name(&self) -> &String {
        return &self.name;
    }

    fn source_string(&self) -> &str {
        return &self.device_path;
    }

    fn formats(&self) -> Vec<Format> {
        let device = Device::with_path(&self.device_path).unwrap();
        let v4l_formats = device.enum_formats().unwrap_or_default();
        let mut formats = vec![];

        debug!("Checking resolutions for camera: {}", &self.device_path);
        for v4l_format in v4l_formats {
            let mut sizes = vec![];
            for v4l_framesizes in device.enum_framesizes(v4l_format.fourcc).unwrap() {
                if let v4l::framesize::FrameSizeEnum::Stepwise(_) = v4l_framesizes.size {
                    warn!("Stepwise framesize not suppported for camera: {}, for configuration: {:#?}",
                        &self.device_path, v4l_framesizes);
                    continue;
                }
                for v4l_size in v4l_framesizes.size.to_discrete() {
                    match &device.enum_frameintervals(
                        v4l_framesizes.fourcc,
                        v4l_size.width,
                        v4l_size.height,
                    ) {
                        Ok(enum_frameintervals) => {
                            let intervals = convert_v4l_intervals(enum_frameintervals);
                            sizes.push(Size {
                                width: v4l_size.width,
                                height: v4l_size.height,
                                intervals,
                            })
                        }
                        Err(error) => {
                            warn!("Failed to fetch frameintervals for camera: {}, for encode: {:?}, for size: {:?}, error: {:#?}",
                                &self.device_path, v4l_format.fourcc, v4l_size, error);
                        }
                    }
                }
            }

            sizes.sort();
            sizes.dedup();
            sizes.iter_mut().for_each(|s| {
                s.intervals.sort();
                s.intervals.reverse();
                s.intervals.dedup();
            });

            formats.push(Format {
                encode: VideoEncodeType::from_str(v4l_format.fourcc.str().unwrap()),
                sizes,
            });
        }

        return formats;
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
        let control = control.unwrap();

        //TODO: Add control validation
        let device = Device::with_path(&self.device_path)?;
        //TODO: we should handle value, value64 and string
        match device.set_control(
            control_id as u32,
            v4l::control::Control::Value(value as i32),
        ) {
            ok @ Ok(_) => ok,
            Err(error) => {
                warn!("Failed to set control {:#?}, error: {:#?}", control, error);
                Err(error)
            }
        }
    }

    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
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

    fn controls(&self) -> Vec<Control> {
        //TODO: create function to encapsulate device
        let device = Device::with_path(&self.device_path).unwrap();
        let v4l_controls = device.query_controls().unwrap_or_default();

        let mut controls: Vec<Control> = vec![];
        for v4l_control in v4l_controls {
            let mut control = Control {
                name: v4l_control.name,
                id: v4l_control.id as u64,
                state: ControlState {
                    is_disabled: v4l_control.flags.contains(v4l::control::Flags::DISABLED),
                    is_inactive: v4l_control.flags.contains(v4l::control::Flags::INACTIVE),
                },
                ..Default::default()
            };
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

    fn is_valid(&self) -> bool {
        return !self.device_path.is_empty();
    }

    fn is_shareable(&self) -> bool {
        return false;
    }
}

impl VideoSourceAvailable for VideoSourceLocal {
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
                debug!(
                    "Failed to capture caps for device: {} {:#?}",
                    camera_path, error
                );
                continue;
            }
            let caps = caps.unwrap();

            if let Err(error) = camera.format() {
                if error.kind() != std::io::ErrorKind::InvalidInput {
                    debug!(
                        "Failed to capture formats for device: {}\nError: {:#?}",
                        camera_path, error
                    );
                }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]

    fn bus_decode() {
        let descriptions = vec![
            (
                // Normal desktop
                VideoSourceLocalType::Usb("usb-0000:08:00.3-1".into()),
                "usb-0000:08:00.3-1",
            ),
            (
                // Normal desktop with additional hubs
                VideoSourceLocalType::Usb("usb-0000:08:00.3-2.1".into()),
                "usb-0000:08:00.3-2.1",
            ),
            (
                // Provided by the raspberry pi with a USB camera
                VideoSourceLocalType::Usb("usb-3f980000.usb-1.4".into()),
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

    #[allow(dead_code)]
    fn simple_test() {
        for camera in VideoSourceLocal::cameras_available() {
            if let VideoSourceType::Local(camera) = camera {
                println!("Camera: {:#?}", camera);
                println!("Resolutions: {:#?}", camera.formats());
                println!("Controls: {:#?}", camera.controls());
            }
        }
    }
}
