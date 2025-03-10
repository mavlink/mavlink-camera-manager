use std::cmp::max;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use lazy_static::lazy_static;
use paperclip::actix::Apiv2Schema;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::*;

use crate::{
    controls::types::*,
    stream::types::VideoCaptureConfiguration,
    video::{
        types::*,
        video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats},
    },
};

lazy_static! {
    static ref VIDEO_FORMATS: Arc<Mutex<HashMap<String, Vec<Format>>>> = Default::default();
}

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

#[instrument(level = "debug")]
fn convert_v4l_intervals(v4l_intervals: &[v4l::FrameInterval]) -> Vec<FrameInterval> {
    let mut intervals: Vec<FrameInterval> = vec![];

    v4l_intervals
        .iter()
        .for_each(|v4l_interval| match &v4l_interval.interval {
            v4l::frameinterval::FrameIntervalEnum::Discrete(fraction) => {
                intervals.push(FrameInterval {
                    numerator: fraction.numerator,
                    denominator: fraction.denominator,
                })
            }
            v4l::frameinterval::FrameIntervalEnum::Stepwise(stepwise) => {
                // To avoid a having a huge number of numerator/denominators, we
                // arbitrarely set a minimum step of 5 units
                let min_step = 5;
                let numerator_step = max(stepwise.step.numerator, min_step);
                let denominator_step = max(stepwise.step.denominator, min_step);

                let numerators = (0..=stepwise.min.numerator)
                    .step_by(numerator_step as usize)
                    .chain(vec![stepwise.max.numerator])
                    .collect::<Vec<u32>>();
                let denominators = (0..=stepwise.min.denominator)
                    .step_by(denominator_step as usize)
                    .chain(vec![stepwise.max.denominator])
                    .collect::<Vec<u32>>();

                for numerator in &numerators {
                    for denominator in &denominators {
                        intervals.push(FrameInterval {
                            numerator: max(1, *numerator),
                            denominator: max(1, *denominator),
                        });
                    }
                }
            }
        });

    intervals.sort();
    intervals.dedup();
    intervals.reverse();

    intervals
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
        let device_path = self.device_path.clone();
        let typ = self.typ.clone();

        unpanic(move || {
            if let Some(formats) = VIDEO_FORMATS.lock().unwrap().get(&device_path) {
                return formats.clone();
            }

            let mut formats = vec![];

            let v4l_device = match v4l::Device::with_path(&device_path) {
                Ok(device) => device,
                Err(error) => {
                    trace!("Faield to get device {device_path:?}: {error:?}");
                    return formats;
                }
            };

            use v4l::video::Capture;

            let v4l_formats = v4l_device.enum_formats().unwrap_or_default();
            trace!("Checking resolutions for camera {device_path:?}");
            for v4l_format in v4l_formats {
                let mut sizes = vec![];
                let mut errors: Vec<String> = vec![];

                let v4l_framesizes = match v4l_device.enum_framesizes(v4l_format.fourcc) {
                    Ok(v4l_framesizes) => v4l_framesizes,
                    Err(error) => {
                        trace!(
                            "Failed to get framesizes from format {v4l_format:?} for device {device_path:?}: {error:#?}"
                        );
                        continue;
                    }
                };

                for v4l_framesize in v4l_framesizes {
                    match v4l_framesize.size {
                        v4l::framesize::FrameSizeEnum::Discrete(v4l_size) => {
                            match &v4l_device.enum_frameintervals(
                                v4l_framesize.fourcc,
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
                                    errors.push(format!(
                                        "encode: {encode:?}, for size: {v4l_size:?}, error: {error:#?}",
                                        encode = v4l_format.fourcc,
                                    ));
                                }
                            }
                        }
                        v4l::framesize::FrameSizeEnum::Stepwise(v4l_size) => {
                            let mut std_sizes: Vec<(u32, u32)> = STANDARD_SIZES.to_vec();
                            std_sizes.push((v4l_size.max_width, v4l_size.max_height));

                            std_sizes.iter().for_each(|(width, height)| {
                                match &v4l_device.enum_frameintervals(v4l_framesize.fourcc, *width, *height) {
                                    Ok(enum_frameintervals) => {
                                        let intervals = convert_v4l_intervals(enum_frameintervals);
                                        sizes.push(Size {
                                            width: *width,
                                            height: *height,
                                            intervals,
                                        });
                                    }
                                    Err(error) => {
                                        errors.push(format!(
                                            "encode: {encode:?}, for size: {v4l_size:?}, error: {error:#?}",
                                            encode = v4l_format.fourcc,
                                            v4l_size = (width, height),
                                        ));
                                    }
                                };
                            });
                        }
                    }
                }

                sizes.sort();
                sizes.dedup();
                sizes.reverse();

                if !errors.is_empty() {
                    trace!("Failed to fetch frameintervals for camera {device_path}: {errors:#?}");
                }

                match v4l_format.fourcc.str() {
                    Ok(encode_str) => {
                        formats.push(Format {
                            encode: encode_str.parse().unwrap(),
                            sizes,
                        });
                    }
                    Err(error) => warn!(
                        "Failed to represent fourcc {:?} as a string: {error:?}",
                        v4l_format.fourcc
                    ),
                }
            }
            // V4l2 reports unsupported sizes for Raspberry Pi
            // Cameras in Legacy Mode, showing the following:
            // > mmal: mmal_vc_port_enable: failed to enable port vc.ril.video_encode:in:0(OPQV): EINVAL
            // > mmal: mmal_port_enable: failed to enable connected port (vc.ril.video_encode:in:0(OPQV))0x75903be0 (EINVAL)
            // > mmal: mmal_connection_enable: output port couldn't be enabled
            // To prevent it, we are currently constraining it
            // to a max. of 1920 x 1080 px, and a max. 30 FPS.
            if matches!(typ, VideoSourceLocalType::LegacyRpiCam(_)) {
                warn!("To support Raspiberry Pi Cameras in Legacy Camera Mode without bugs, resolution is constrained to 1920 x 1080 @ 30 FPS.");
                let max_width = 1920;
                let max_height = 1080;
                let max_fps = 30;
                formats.iter_mut().for_each(|format| {
                    format.sizes.iter_mut().for_each(|size| {
                        if size.width > max_width {
                            size.width = max_width;
                        }

                        if size.height > max_height {
                            size.height = max_height;
                        }

                        size.intervals = size
                            .intervals
                            .clone()
                            .into_iter()
                            .filter(|interval| interval.numerator * interval.denominator <= max_fps)
                            .collect();
                    });

                    format.sizes.dedup();
                });
            }
            formats.sort();
            formats.dedup();

            VIDEO_FORMATS
                .lock()
                .unwrap()
                .insert(device_path.to_string(), formats.clone());
            formats
        })
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
            unsupported_type => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
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
        let mut cameras: Vec<VideoSourceType> = vec![];

        let cameras_path = {
            let read_dir = match std::fs::read_dir("/dev/") {
                Ok(cameras_path) => cameras_path,
                Err(error) => {
                    error!("Failed to get cameras from \"/dev/\": {error:?}");
                    return cameras;
                }
            };

            read_dir
                .filter_map(|dir_entry| {
                    let dir_entry = match dir_entry {
                        Ok(dir_entry) => dir_entry,
                        Err(error) => {
                            debug!("Failed reading a device: {error:?}");
                            return None;
                        }
                    };

                    dir_entry.path().to_str().map(String::from)
                })
                .filter(|entry_str| entry_str.starts_with("/dev/video"))
                .collect::<Vec<String>>()
        };

        for camera_path in &cameras_path {
            let Ok(caps) = unpanic({
                use v4l::video::Capture;

                let camera_path = camera_path.clone();
                move || {
                    let v4l_device = v4l::Device::with_path(&camera_path).inspect_err(|error| {
                        trace!("Failed to get device {camera_path:?}. Reason: {error:?}")
                    })?;

                    let caps = v4l_device.query_caps().inspect_err(|error| {
                        trace!(
                            "Failed to capture caps for device: {camera_path:?}. Reason: {error:?}"
                        )
                    })?;

                    v4l_device.format().inspect_err(|error| {
                        trace!("Failed to capture formats for device: {camera_path:?}. Reason: {error:?}")
                    })?;

                    Ok::<_, std::io::Error>(caps)
                }
            }) else {
                continue;
            };

            let typ = VideoSourceLocalType::from_str(&caps.bus);

            let source = VideoSourceLocal {
                name: caps.card,
                device_path: camera_path.to_string(),
                typ,
            };
            cameras.push(VideoSourceType::Local(source));
        }

        cameras
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
    use serial_test::serial;
    use tracing_test::traced_test;

    use super::*;
    use crate::{
        stream::types::{CaptureConfiguration, StreamInformation},
        video_stream::types::VideoAndStreamInformation,
    };
    use VideoEncodeType::*;

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
            typ: VideoSourceLocalType::Usb(usb_bus.into()),
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
