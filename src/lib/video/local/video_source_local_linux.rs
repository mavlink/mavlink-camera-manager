use std::{
    cmp::max,
    collections::{HashMap, HashSet},
    str::FromStr,
};

use anyhow::{anyhow, Result};
use gst::prelude::*;
use paperclip::actix::Apiv2Schema;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::*;

use crate::{
    controls::types::*,
    stream::types::VideoCaptureConfiguration,
    video::{
        gst_device_monitor,
        types::*,
        video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats},
    },
};

/// Helper function to wrap calls from v4l that can cause panic, returning an error instead
fn unpanic<T, F>(body: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::Builder::new()
        .name("v4l_wrap".to_string())
        .spawn(body)
        .expect("Failed to spawn thread")
        .join()
        .inspect_err(|e| error!("v4l API failed with: {:?}", e.downcast_ref::<String>()))
        .unwrap()
}

//TODO: Move to types
#[derive(Apiv2Schema, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum VideoSourceLocalType {
    Unknown(String),
    Usb(String),
    LegacyRpiCam(String),
}

#[derive(Apiv2Schema, Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
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

    #[instrument(level = "debug")]
    pub fn from_str(description: &str) -> Self {
        if let Some(result) = VideoSourceLocalType::usb_from_str(description) {
            return result;
        }

        if let Some(result) = VideoSourceLocalType::v4l2_from_str(description) {
            return result;
        }

        let msg = format!("Unable to identify the local camera connection type, please report the problem: {description:?}");
        if description == "platform:bcm2835-isp" {
            // Filter out the log for this particular device, because regarding to Raspberry Pis, it will always be there and we will never use it.
            trace!(msg);
        } else {
            warn!(msg);
        }
        VideoSourceLocalType::Unknown(description.into())
    }

    #[instrument(level = "debug")]
    fn usb_from_str(description: &str) -> Option<Self> {
        let regex = match Regex::new(
            r"usb-(?P<interface>(([0-9a-fA-F]{2}){1,2}:?){4})?\.(usb-)?(?P<device>.*)",
        ) {
            Ok(regex) => regex,
            Err(error) => {
                error!("Failed to construct regex: {error:?}");
                return None;
            }
        };

        if regex.is_match(description) {
            return Some(VideoSourceLocalType::Usb(description.into()));
        }
        None
    }

    #[instrument(level = "debug")]
    fn v4l2_from_str(description: &str) -> Option<Self> {
        let regex = match Regex::new(r"platform:(?P<device>\S+)-v4l2-[0-9]") {
            Ok(regex) => regex,
            Err(error) => {
                error!("Failed to construct regex: {error:?}");
                return None;
            }
        };

        if regex.is_match(description) {
            return Some(VideoSourceLocalType::LegacyRpiCam(description.into()));
        }
        None
    }
}

impl VideoSourceLocal {
    #[instrument(level = "debug")]
    pub async fn try_identify_device(
        &mut self,
        capture_configuration: &VideoCaptureConfiguration,
        candidates: &[VideoSourceType],
    ) -> Result<Option<String>> {
        // Rule n.1 - All candidates must share the same camera name
        let candidates = Self::get_cameras_with_same_name(candidates, &self.name);

        let len = candidates.len();
        if len == 0 {
            // Outcome n.1 - This happens when all cameras connected has changed since last settings
            trace!("Outcome n.1!");
            return Err(anyhow!("Device not found"));
        }

        // Rule n.2 - All candidates must share the same encode
        let candidates =
            Self::get_cameras_with_same_encode(&candidates, &capture_configuration.encode).await;

        let len = candidates.len();
        if len == 0 {
            // Outcome n.2 - This could only happen if there are cameras (not devices) with the same name
            // but different encodes, maybe if the kernel fail to add (or remove) one of the camera's linux
            // devices, for example, for a camera that originally has two devices, say `/dev/video0` (with
            // only H264 encode), and `/dev/video1` (with YUYV and MJPG encodes), and for any reason,
            // suddently `/dev/video1` is unmounted, then we'd be invalidating `/dev/video1` while keeping
            // `/dev/video0`.
            trace!("Outcome n.2!");
            return Err(anyhow!("Device not found"));
        }
        if len == 1 {
            // Outcome n.3 - This happens when we change cameras from one USB port to another
            // This can happen ocasionally, by chance, when the kernel enumerates the devices
            // in a different order making, say, `/dev/video0` (initially with H264 encode)
            // appear interchangibly as `/dev/video1`, which initially had YUYV and MJPG
            // encodes. This can happens everytime we reconnect a USB, or restart the OS.
            trace!("Outcome n.3!");
            let candidate = &candidates[0];
            return Ok(Some(candidate.inner().source_string().to_string()));
        }

        // Rule n.3 - Same name, same encode, same USB port.
        let candidates = Self::get_cameras_with_same_bus(&candidates, &self.typ);

        let len = candidates.len();
        if len == 1 {
            trace!("Outcome n.4!");
            let candidate = &candidates[0];
            return Ok(Some(candidate.inner().source_string().to_string()));
        }

        // Outcome n.5 - If there are several candidates left, and as we lack other methods to diferentiate
        // them, it's better to not change anything, keeping their identity as it is. This is the case when
        // we have two or more identical USB cameras connected, let's say we have, initially, two cameras
        // connected:
        // - USB port A with camera `Alpha`, receiving devices /dev/video0 (H264) and /dev/video1 (MJPG, YUYV)
        // - USB port B with camera `Beta`, receiving devices /dev/video2 (H264) and /dev/video3 (MJPG, YUYV).
        // Then in the next reboot, `Alpha` changed to port B, and `Beta` was changed to port A, then it's
        // impossible to differentiate them using only the name.
        trace!("Outcome n.5!");
        warn!("There is more than one camera with the same name and encode, which means that their identification/configurations could have been swaped");
        Ok(None)
    }

    #[instrument(level = "debug")]
    fn get_cameras_with_same_name(
        candidates: &[VideoSourceType],
        name: &str,
    ) -> Vec<VideoSourceType> {
        candidates
            .iter()
            .filter(|candidate| {
                let VideoSourceType::Local(camera) = candidate else {
                    return false;
                };

                camera.name == name
            })
            .cloned()
            .collect()
    }

    #[instrument(level = "debug")]
    async fn get_cameras_with_same_encode(
        candidates: &[VideoSourceType],
        encode: &VideoEncodeType,
    ) -> Vec<VideoSourceType> {
        let mut result = Vec::new();

        for candidate in candidates.iter() {
            if candidate
                .formats()
                .await
                .iter()
                .any(|format| &format.encode == encode)
            {
                result.push(candidate.clone());
            }
        }

        result
    }

    #[instrument(level = "debug")]
    fn get_cameras_with_same_bus(
        candidates: &[VideoSourceType],
        typ: &VideoSourceLocalType,
    ) -> Vec<VideoSourceType> {
        candidates
            .iter()
            .filter(|candidate| {
                let VideoSourceType::Local(camera) = candidate else {
                    return false;
                };
                &camera.typ == typ
            })
            .cloned()
            .collect()
    }
}

fn compute_intervals_from_range(
    interval_start: FrameInterval,
    interval_end: FrameInterval,
    step: usize,
) -> Vec<FrameInterval> {
    let mut intervals = Vec::with_capacity(20);

    // To avoid having a huge number of numerator/denominators, we
    // arbitrarily set a minimum step of 5 units
    let step = step.max(5);

    let numerator_end = interval_end.numerator.min(30);
    let denominator_end = interval_end.denominator.min(30);

    let min_numerator = max(1, interval_start.numerator);
    let min_denominator = max(1, interval_start.denominator);

    for numerator in (0..=numerator_end).step_by(step) {
        for denominator in (0..=denominator_end).step_by(step) {
            intervals.push(FrameInterval {
                numerator: numerator.max(min_numerator),
                denominator: denominator.max(min_denominator),
            });
        }
    }

    intervals
}

impl From<gst::Fraction> for FrameInterval {
    fn from(value: gst::Fraction) -> Self {
        FrameInterval {
            // Yes, our nominator is GST's denominator.
            numerator: value.denom() as u32,
            denominator: value.numer() as u32,
        }
    }
}

fn get_device_formats_using_gstreamer(
    device_path: &str,
    _typ: &VideoSourceLocalType,
) -> Result<Vec<Format>> {
    let device = gst_device_monitor::v4l_device_with_path(device_path)?;

    let caps = gst_device_monitor::device_caps(&device)?;

    let mut sizes_by_encode: HashMap<VideoEncodeType, HashSet<Size>> = HashMap::new();

    caps.iter().for_each(|structure| {
        let encode = match structure.name().as_str() {
            "video/x-raw" => {
                let fourcc = structure.get::<String>("format").unwrap();
                VideoEncodeType::from_str(&fourcc).expect("irrefutable")
            }
            "image/jpeg" => VideoEncodeType::Mjpg,
            "video/x-h264" => VideoEncodeType::H264,
            "video/x-h265" => VideoEncodeType::H265,
            other => {
                info!("unknown format: {other:?}");

                return;
            }
        };

        let mut heights = match structure.value("height") {
            Ok(sendvalue) => match sendvalue.type_().name() {
                "gint" => vec![sendvalue.get::<i32>().unwrap() as u32],
                "GstIntRange" => {
                    let range = sendvalue.get::<gst::IntRange<i32>>().unwrap();

                    let start = range.min() as u32;
                    let end = range.max() as u32;
                    let step = range.step() as u32;

                    STANDARD_SIZES
                        .iter()
                        .filter_map(|(_, height)| {
                            if height >= &start && height <= &end && (height % step == 0) {
                                return Some(*height);
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                }
                "GstValueList" => sendvalue
                    .get::<gst::List>()
                    .unwrap()
                    .iter()
                    .map(|v| v.get::<i32>().unwrap() as u32)
                    .collect::<Vec<_>>(),
                unsupported_type => {
                    info!("Height with unsupported type: {unsupported_type:?}, {structure:#?}");
                    return;
                }
            },
            Err(error) => {
                info!("No height: {structure:#?}: {error:?}");
                return;
            }
        };

        let mut widths = match structure.value("width") {
            Ok(sendvalue) => match sendvalue.type_().name() {
                "gint" => vec![sendvalue.get::<i32>().unwrap() as u32],
                "GstIntRange" => {
                    let range = sendvalue.get::<gst::IntRange<i32>>().unwrap();

                    let start = range.min() as u32;
                    let end = range.max() as u32;
                    let step = range.step() as u32;

                    STANDARD_SIZES
                        .iter()
                        .filter_map(|(width, _)| {
                            if width >= &start && width <= &end && (width % step == 0) {
                                return Some(*width);
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                }
                "GstValueList" => sendvalue
                    .get::<gst::List>()
                    .unwrap()
                    .iter()
                    .map(|v| v.get::<i32>().unwrap() as u32)
                    .collect::<Vec<_>>(),
                unsupported_type => {
                    info!("Width with unsupported type: {unsupported_type:?}, {structure:#?}");
                    return;
                }
            },
            Err(error) => {
                info!("No width: {structure:#?}: {error:?}");
                return;
            }
        };

        let mut intervals = match structure.value("framerate") {
            Ok(sendvalue) => match sendvalue.type_().name() {
                "GstFraction" => {
                    vec![sendvalue.get::<gst::Fraction>().unwrap().into()]
                }
                "GstFractionRange" => {
                    let range = sendvalue.get::<gst::FractionRange>().unwrap();

                    compute_intervals_from_range(range.min().into(), range.max().into(), 1)
                }
                "GstValueList" => sendvalue
                    .get::<gst::List>()
                    .unwrap()
                    .iter()
                    .map(|sendvalue| sendvalue.get::<gst::Fraction>().unwrap().into())
                    .collect::<Vec<_>>(),
                unsupported_type => {
                    info!("Framerate with unsupported type: {unsupported_type:?}, {structure:#?}");
                    return;
                }
            },
            Err(error) => {
                info!("No framerate: {structure:#?}: {error:?}");
                return;
            }
        };

        heights.dedup();
        heights.sort();

        widths.dedup();
        widths.sort();

        intervals.sort();
        intervals.dedup();
        intervals.reverse();

        widths.into_iter().zip(heights).for_each(|(width, height)| {
            let size = Size {
                width,
                height,
                intervals: intervals.clone(),
            };

            if let Some(size_set) = sizes_by_encode.get_mut(&encode) {
                size_set.insert(size);
                return;
            }

            sizes_by_encode.insert(encode.clone(), HashSet::from([size]));
        });
    });

    let mut formats = Vec::with_capacity(sizes_by_encode.len());

    sizes_by_encode.into_iter().for_each(|(encode, sizes)| {
        let mut sizes = sizes.into_iter().collect::<Vec<Size>>();
        sizes.sort();
        // sizes.dedup();
        sizes.reverse();

        formats.push(Format { encode, sizes })
    });

    Ok(formats)
}

#[instrument(level = "debug")]
fn validate_control(control: &Control, value: i64) -> Result<(), String> {
    if control.state.is_inactive {
        return Err("Control is inactive".to_string());
    } else if control.state.is_disabled {
        return Err("Control is disabled".to_string());
    }

    match &control.configuration {
        ControlType::Slider(control) => {
            if value > control.max {
                return Err(format!(
                    "Value {value:?} is greater than the Control maximum value: {:?}",
                    control.max
                ));
            } else if value < control.min {
                return Err(format!(
                    "Value {value:?} is lower than the Control minimum value: {:?}",
                    control.min
                ));
            }
        }
        ControlType::Menu(control) => {
            if !control.options.iter().any(|opt| opt.value == value) {
                return Err(format!(
                    "Value {value:?} is not one of the Control options: {:?}",
                    control.options
                ));
            }
        }
        ControlType::Bool(_) => {
            let values = &[0, 1];
            if !values.contains(&value) {
                return Err(format!(
                    "Value {value:?} is not one of the Control accepted values: {values:?}"
                ));
            }
        }
    }

    Ok(())
}

impl VideoSourceFormats for VideoSourceLocal {
    #[instrument(level = "debug")]
    async fn formats(&self) -> Vec<Format> {
        let device_path = &self.device_path;
        let typ = &self.typ;

        return match get_device_formats_using_gstreamer(device_path, typ) {
            Ok(devices) => devices,
            Err(error) => {
                warn!("Failed getting formats for device {device_path:?}: {error:?}");
                vec![]
            }
        };
    }
}

impl VideoSource for VideoSourceLocal {
    #[instrument(level = "debug")]
    fn name(&self) -> &String {
        &self.name
    }

    #[instrument(level = "debug")]
    fn source_string(&self) -> &str {
        &self.device_path
    }

    #[instrument(level = "debug")]
    fn set_control_by_name(&self, control_name: &str, value: i64) -> std::io::Result<()> {
        let Some(control_id) = self
            .controls()
            .iter()
            .find_map(|control| (control.name == control_name).then_some(control.id))
        else {
            let names: Vec<String> = self
                .controls()
                .into_iter()
                .map(|control| control.name)
                .collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Control named {control_name:?} was not found, options are: {names:?}"),
            ));
        };

        self.set_control_by_id(control_id, value)
    }

    #[instrument(level = "debug")]
    fn set_control_by_id(&self, control_id: u64, value: i64) -> std::io::Result<()> {
        let Some(control) = self
            .controls()
            .into_iter()
            .find(|control| control.id == control_id)
        else {
            let ids: Vec<u64> = self.controls().iter().map(|control| control.id).collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Control ID {control_id:?} was not found, options are: {ids:?}"),
            ));
        };

        if let Err(error) = validate_control(&control, value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("Failed setting {control_id:?} to {value:?}: {error}"),
            ));
        }

        let device_path = self.device_path.clone();

        let v4l_device = unpanic(move || v4l::Device::with_path(device_path))?;

        //TODO: we should handle value, value64 and string
        let v4l_control = v4l::Control {
            id: control_id as u32,
            value: v4l::control::Value::Integer(value),
        };

        match unpanic(move || v4l_device.set_control(v4l_control)) {
            ok @ Ok(_) => ok,
            Err(error) => {
                trace!("Failed to set control {control:#?}, error: {error:#?}");
                Err(error)
            }
        }
    }

    fn control_value_by_name(&self, control_name: &str) -> std::io::Result<i64> {
        let Some(control_id) = self
            .controls()
            .iter()
            .find_map(|control| (control.name == control_name).then_some(control.id))
        else {
            let names: Vec<String> = self
                .controls()
                .into_iter()
                .map(|control| control.name)
                .collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Control named {control_name:?} was not found, options are: {names:?}"),
            ));
        };

        self.control_value_by_id(control_id)
    }

    #[instrument(level = "debug")]
    fn control_value_by_id(&self, control_id: u64) -> std::io::Result<i64> {
        let device_path = self.device_path.clone();

        let v4l_device = unpanic(move || v4l::Device::with_path(device_path))?;

        let control = unpanic(move || v4l_device.control(control_id as u32))?;

        match control.value {
            v4l::control::Value::Integer(value) => Ok(value),
            v4l::control::Value::Boolean(value) => Ok(value as i64),
            unsupported_type => Err(std::io::Error::other(
                format!("Control type {unsupported_type:?} is not supported.").as_str(),
            )),
        }
    }

    #[instrument(level = "debug")]
    fn controls(&self) -> Vec<Control> {
        let mut controls: Vec<Control> = vec![];

        //TODO: create function to encapsulate device
        let device_path = self.device_path.clone();
        let v4l_device = match unpanic(move || v4l::Device::with_path(device_path)) {
            Ok(device) => device,
            Err(error) => {
                trace!("Faield to get device {:?}: {error:?}", self.device_path);
                return controls;
            }
        };

        let v4l_controls = match unpanic(move || v4l_device.query_controls()) {
            Ok(device) => device,
            Err(error) => {
                trace!(
                    "Faield to get controls for device {:?}: {error:?}",
                    self.device_path
                );
                return controls;
            }
        };

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

            if matches!(v4l_control.typ, v4l::control::Type::CtrlClass) {
                // CtrlClass is not a control, so we are skipping it to avoid any access to it, as it will raise an
                // IO error #13: Permission Denied. To better understand, look for 'V4L2_CTRL_TYPE_CTRL_CLASS' on
                // this doc: https://www.kernel.org/doc/html/v5.5/media/uapi/v4l/vidioc-queryctrl.html#c.v4l2_ctrl_type
                continue;
            }

            let value = match self.control_value_by_id(v4l_control.id as u64) {
                Ok(value) => value,
                Err(error) => {
                    error!(
                        "Failed to get control {:?} ({:?}) from device {:?}: {error:?}",
                        control.name, control.id, &self.device_path
                    );
                    continue;
                }
            };
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
        controls
    }

    #[instrument(level = "debug")]
    fn is_valid(&self) -> bool {
        !self.device_path.is_empty()
    }

    #[instrument(level = "debug")]
    fn is_shareable(&self) -> bool {
        false
    }
}

impl VideoSourceAvailable for VideoSourceLocal {
    #[instrument(level = "debug")]
    async fn cameras_available() -> Vec<VideoSourceType> {
        gst_device_monitor::v4l_devices()
            .unwrap_or_default()
            .iter()
            .filter_map(|device_weak| {
                let device = device_weak.upgrade()?;
                let name = device.display_name().to_string();
                let properties = device.properties()?;
                let device_path = properties.get::<String>("device.path").ok()?;
                let bus = properties.get::<String>("v4l2.device.bus_info").ok()?;

                let typ = VideoSourceLocalType::Usb(bus.to_string());
                Some(VideoSourceType::Local(VideoSourceLocal {
                    name,
                    device_path,
                    typ,
                }))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::*;

    #[traced_test]
    #[instrument(level = "debug")]
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
                // Provided by the raspberry pi with a Raspberry Pi camera when in to use legacy camera mode
                VideoSourceLocalType::LegacyRpiCam("platform:bcm2835-v4l2-0".into()),
                "platform:bcm2835-v4l2-0",
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
}

#[cfg(test)]
mod device_identification_tests {
    use std::sync::{Arc, Mutex};

    use lazy_static::lazy_static;
    use serial_test::serial;
    use tracing_test::traced_test;

    use super::*;
    use crate::{
        stream::types::{CaptureConfiguration, StreamInformation},
        video_stream::types::VideoAndStreamInformation,
    };
    use VideoEncodeType::*;

    lazy_static! {
        static ref VIDEO_FORMATS: Arc<Mutex<HashMap<String, Vec<Format>>>> = Default::default();
    }

    #[instrument(level = "debug")]
    fn add_available_camera(
        name: &str,
        device_path: &str,
        usb_bus: &str,
        encodes: Vec<VideoEncodeType>,
    ) -> VideoSourceType {
        let formats = encodes
            .into_iter()
            .map(|encode| Format {
                encode,
                sizes: vec![Size {
                    width: 1920,
                    height: 1080,
                    intervals: vec![FrameInterval {
                        numerator: 30,
                        denominator: 1,
                    }],
                }],
            })
            .collect::<Vec<Format>>();

        // Register its formats
        VIDEO_FORMATS
            .lock()
            .unwrap()
            .insert(device_path.to_string(), formats);

        VideoSourceType::Local(VideoSourceLocal {
            name: name.into(),
            device_path: device_path.into(),
            typ: VideoSourceLocalType::Usb((&*usb_bus).into()),
        })
    }

    #[instrument(level = "debug")]
    fn create_stream(
        name: &str,
        device_path: &str,
        usb_bus: &str,
        encode: VideoEncodeType,
    ) -> VideoAndStreamInformation {
        VideoAndStreamInformation {
            name: "dummy stream".into(),
            stream_information: StreamInformation {
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode,
                    height: 1080,
                    width: 1920,
                    frame_interval: FrameInterval {
                        numerator: 30,
                        denominator: 1,
                    },
                }),
                endpoints: vec![url::Url::parse("udp://0.0.0.0:5600").unwrap()],
                extended_configuration: None,
            },
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: name.into(),
                device_path: device_path.into(),
                typ: VideoSourceLocalType::Usb(usb_bus.into()),
            }),
        }
    }

    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn test_get_cameras_with_same_name() {
        VIDEO_FORMATS.lock().unwrap().clear();

        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0", vec![H264]),
            add_available_camera("A", "/dev/video1", "usb_port_0", vec![Yuyv, Mjpg]),
            add_available_camera("A", "/dev/video2", "usb_port_1", vec![H264]),
            add_available_camera("A", "/dev/video3", "usb_port_1", vec![Yuyv, Mjpg]),
            add_available_camera("B", "/dev/video4", "usb_port_2", vec![H264]),
            add_available_camera("B", "/dev/video5", "usb_port_2", vec![Yuyv, Mjpg]),
        ];

        let same_name_candidates = VideoSourceLocal::get_cameras_with_same_name(&candidates, "A");
        assert_eq!(candidates[..4].to_vec(), same_name_candidates);

        VIDEO_FORMATS.lock().unwrap().clear();
    }

    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn test_get_cameras_with_same_encode() {
        VIDEO_FORMATS.lock().unwrap().clear();

        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0", vec![H264]),
            add_available_camera("B", "/dev/video1", "usb_port_1", vec![H264]),
            add_available_camera("C", "/dev/video2", "usb_port_0", vec![Yuyv, Mjpg]),
            add_available_camera("D", "/dev/video3", "usb_port_1", vec![Yuyv, Mjpg]),
        ];

        let same_encode_candidates =
            VideoSourceLocal::get_cameras_with_same_encode(&candidates, &H264).await;
        assert_eq!(candidates[..2].to_vec(), same_encode_candidates);

        VIDEO_FORMATS.lock().unwrap().clear();
    }

    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn test_get_cameras_with_same_bus() {
        VIDEO_FORMATS.lock().unwrap().clear();

        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0", vec![H264]),
            add_available_camera("B", "/dev/video1", "usb_port_0", vec![Yuyv, Mjpg]),
            add_available_camera("C", "/dev/video2", "usb_port_1", vec![H264]),
            add_available_camera("D", "/dev/video3", "usb_port_1", vec![Yuyv, Mjpg]),
        ];

        let same_encode_candidates = VideoSourceLocal::get_cameras_with_same_bus(
            &candidates,
            &VideoSourceLocalType::Usb("usb_port_0".into()),
        );
        assert_eq!(candidates[..2].to_vec(), same_encode_candidates);

        VIDEO_FORMATS.lock().unwrap().clear();
    }

    // #[traced_test]
    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn identify_a_candidate_with_same_name_and_encode() {
        VIDEO_FORMATS.lock().unwrap().clear();

        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0", vec![H264]),
            add_available_camera("A", "/dev/video1", "usb_port_0", vec![Yuyv, Mjpg]),
            add_available_camera("B", "/dev/video2", "usb_port_1", vec![H264]),
            add_available_camera("B", "/dev/video3", "usb_port_1", vec![Yuyv, Mjpg]),
            add_available_camera("C", "/dev/video3", "usb_port_1", vec![H264, Yuyv, Mjpg]),
        ];
        let stream = create_stream("A", "/dev/video0", "usb_port_0", H264);
        let (VideoSourceType::Local(source), CaptureConfiguration::Video(capture_configuration)) = (
            &stream.video_source,
            &stream.stream_information.configuration,
        ) else {
            unreachable!("Wrong setup")
        };

        let Ok(Some(candidate_source_string)) = source
            .to_owned()
            .try_identify_device(capture_configuration, &candidates)
            .await
        else {
            panic!("Failed to identify the only device with the same name and encode")
        };

        assert_eq!(
            candidate_source_string,
            stream.video_source.inner().source_string().to_string()
        );

        // IF we remove the only device with the same name and encode, we should get an error
        source
            .to_owned()
            .try_identify_device(capture_configuration, &candidates[1..])
            .await
            .expect_err("Failed to identify the only device with the same name and encode");

        VIDEO_FORMATS.lock().unwrap().clear();
    }

    // #[traced_test]
    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn identify_a_candidate_when_usb_port_changed() {
        VIDEO_FORMATS.lock().unwrap().clear();

        // Before this boot, the device candidates[0] was in "usb_port_0" and the device candidates[1] was in "usb_port_1":
        let last_usb_bus = "usb_port_1";
        let current_usb_bus = "usb_port_0";

        let candidates = vec![
            add_available_camera("A", "/dev/video0", current_usb_bus, vec![H264]),
            add_available_camera("A", "/dev/video1", current_usb_bus, vec![Yuyv, Mjpg]),
            add_available_camera("B", "/dev/video2", "usb_port_3", vec![H264]),
            add_available_camera("B", "/dev/video3", "usb_port_3", vec![Yuyv, Mjpg]),
        ];

        for n in (0..3).collect::<Vec<_>>() {
            let stream = create_stream("A", &format!("/dev/video{n}"), last_usb_bus, H264);
            let (
                VideoSourceType::Local(source),
                CaptureConfiguration::Video(capture_configuration),
            ) = (
                &stream.video_source,
                &stream.stream_information.configuration,
            )
            else {
                unreachable!("Wrong setup")
            };

            let Ok(Some(candidate_source_string)) = source
                .to_owned()
                .try_identify_device(capture_configuration, &candidates)
                .await
            else {
                panic!("Failed to identify the only device with the same name and encode")
            };
            assert_eq!(
                candidate_source_string,
                candidates[0].inner().source_string()
            );

            // If we remove the only device with the same name and encode, we should get an error
            let mut other_candidates = candidates.clone();
            other_candidates.remove(0);
            source
                .to_owned()
                .try_identify_device(capture_configuration, &other_candidates)
                .await
                .expect_err("Failed to identify the only device with the same name and encode");
        }

        VIDEO_FORMATS.lock().unwrap().clear();
    }

    // #[traced_test]
    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn identify_a_candidate_when_path_changed() {
        VIDEO_FORMATS.lock().unwrap().clear();

        // Before this boot, the device candidates[0] was in "/dev/video1" and the device candidates[1] was in "/dev/video0":
        let last_path = "/dev/video1";
        let current_path = "/dev/video0";

        let candidates = vec![
            add_available_camera("A", current_path, "usb_port_0", vec![H264]),
            add_available_camera("A", last_path, "usb_port_1", vec![H264]),
            add_available_camera("A", "/dev/video3", "usb_port_0", vec![Yuyv, Mjpg]),
            add_available_camera("A", "/dev/video5", "usb_port_1", vec![Yuyv, Mjpg]),
        ];

        for n in (0..=1).collect::<Vec<_>>() {
            let stream = create_stream("A", last_path, &format!("usb_port_{n}"), H264);

            let (
                VideoSourceType::Local(source),
                CaptureConfiguration::Video(capture_configuration),
            ) = (
                &stream.video_source,
                &stream.stream_information.configuration,
            )
            else {
                unreachable!("Wrong setup")
            };

            let Ok(Some(candidate_source_string)) = source
                .to_owned()
                .try_identify_device(capture_configuration, &candidates)
                .await
            else {
                panic!("Failed to identify the only device with the same name and encode")
            };
            assert_eq!(
                candidate_source_string,
                candidates[n].inner().source_string()
            );
        }

        VIDEO_FORMATS.lock().unwrap().clear();
    }

    // #[traced_test]
    #[serial]
    #[traced_test]
    #[tokio::test]
    async fn do_not_identify_if_several_devices_with_same_name_and_encode() {
        VIDEO_FORMATS.lock().unwrap().clear();

        // Before this boot, the device candidates[0] was in "usb_port_0" and the device candidates[1] was in "usb_port_1":
        let last_usb_bus = "usb_port_1";
        let current_usb_bus = "usb_port_0";

        let candidates = vec![
            add_available_camera("A", "/dev/video0", current_usb_bus, vec![H264]),
            add_available_camera("A", "/dev/video1", current_usb_bus, vec![Yuyv, Mjpg]),
            add_available_camera("A", "/dev/video4", "usb_port_2", vec![H264]),
            add_available_camera("A", "/dev/video5", "usb_port_2", vec![Yuyv, Mjpg]),
        ];

        for n in (0..5).collect::<Vec<_>>() {
            let stream = create_stream("A", &format!("/dev/video{n}"), last_usb_bus, H264);
            let (
                VideoSourceType::Local(source),
                CaptureConfiguration::Video(capture_configuration),
            ) = (
                &stream.video_source,
                &stream.stream_information.configuration,
            )
            else {
                unreachable!("Wrong setup")
            };

            assert!(source
                .to_owned()
                .try_identify_device(capture_configuration, &candidates)
                .await
                .expect("Failed to identify the only device with the same name and encode")
                .is_none())
        }

        VIDEO_FORMATS.lock().unwrap().clear();
    }
}
