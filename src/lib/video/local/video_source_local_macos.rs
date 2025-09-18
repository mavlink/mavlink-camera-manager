use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use lazy_static::lazy_static;
use paperclip::actix::Apiv2Schema;
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

#[cfg(target_os = "macos")]
use nokhwa::{
    Camera,
    utils::{
        CameraIndex,
        KnownCameraControl,
        ControlValueSetter,
        CameraInfo,
        RequestedFormat,
        RequestedFormatType,
        Resolution,
        FrameFormat,
    },
    pixel_format::YuyvFormat,
};

lazy_static! {
    static ref VIDEO_FORMATS: Arc<Mutex<HashMap<String, Vec<Format>>>> = Default::default();
}

/// Helper function to wrap calls from nokhwa that can cause panic, returning an error instead
fn unpanic<T, F>(body: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::Builder::new()
        .name("nokhwa_wrap".to_string())
        .spawn(body)
        .expect("Failed to spawn thread")
        .join()
        .inspect_err(|e| error!("Nokhwa API failed with: {:?}", e.downcast_ref::<String>()))
        .unwrap()
}

//TODO: Move to types
#[derive(Apiv2Schema, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum VideoSourceLocalType {
    Unknown(String),
    Usb(String),
    BuiltIn(String),
}

#[derive(Apiv2Schema, Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct VideoSourceLocal {
    pub name: String,
    pub device_path: String,
    #[serde(rename = "type")]
    pub typ: VideoSourceLocalType,
    pub device_index: usize,
}

impl VideoSourceLocalType {
    #[instrument(level = "debug")]
    pub fn from_camera_info(info: &CameraInfo) -> Self {
        let description = info.human_name();

        if description.contains("USB") || description.contains("USB2.0") {
            VideoSourceLocalType::Usb(description)
        } else if description.contains("Built-in") ||
                  description.contains("FaceTime") ||
                  description.contains("iSight") ||
                  description.contains("HD Camera") {
            VideoSourceLocalType::BuiltIn(description)
        } else {
            let msg = format!("Unable to identify the local camera connection type, please report the problem: {description:?}");
            warn!(msg);
            VideoSourceLocalType::Unknown(description)
        }
    }

    #[instrument(level = "debug")]
    pub fn from_str(description: &str) -> Self {
        if description.contains("USB") {
            VideoSourceLocalType::Usb(description.into())
        } else if description.contains("Built-in") || description.contains("FaceTime") {
            VideoSourceLocalType::BuiltIn(description.into())
        } else {
            let msg = format!("Unable to identify the local camera connection type, please report the problem: {description:?}");
            warn!(msg);
            VideoSourceLocalType::Unknown(description.into())
        }
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
            // but different encodes
            trace!("Outcome n.2!");
            return Err(anyhow!("Device not found"));
        }
        if len == 1 {
            // Outcome n.3 - This happens when we change cameras from one port to another
            trace!("Outcome n.3!");
            let candidate = &candidates[0];
            return Ok(Some(candidate.inner().source_string().to_string()));
        }

        // Rule n.3 - Same name, same encode, same connection type.
        let candidates = Self::get_cameras_with_same_bus(&candidates, &self.typ);

        let len = candidates.len();
        if len == 1 {
            trace!("Outcome n.4!");
            let candidate = &candidates[0];
            return Ok(Some(candidate.inner().source_string().to_string()));
        }

        // Outcome n.5 - If there are several candidates left, and as we lack other methods to differentiate
        // them, it's better to not change anything, keeping their identity as it is.
        trace!("Outcome n.5!");
        warn!("There is more than one camera with the same name and encode, which means that their identification/configurations could have been swapped");
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
fn convert_nokhwa_format_to_video_encode(frame_format: FrameFormat) -> VideoEncodeType {
    match frame_format {
        FrameFormat::YUYV => VideoEncodeType::Yuyv,
        FrameFormat::MJPEG => VideoEncodeType::Mjpg,
        FrameFormat::RAWRGB => VideoEncodeType::Rgb,
        FrameFormat::GRAY => VideoEncodeType::Rgb, // Treat grayscale as RGB
        FrameFormat::NV12 => VideoEncodeType::Unknown("NV12".to_string()),
        FrameFormat::RAWBGR => VideoEncodeType::Unknown("RAWBGR".to_string()),
    }
}

#[instrument(level = "debug")]
fn convert_nokhwa_resolution_to_sizes(resolution: Resolution) -> Vec<Size> {
    let mut sizes = vec![];

    // Convert common FPS values to FrameInterval
    let common_fps = [15.0, 24.0, 25.0, 30.0, 50.0, 60.0];

    for fps_val in common_fps {
        let intervals = vec![FrameInterval {
            numerator: fps_val as u32,
            denominator: 1,
        }];

        sizes.push(Size {
            width: resolution.width(),
            height: resolution.height(),
            intervals,
        });
    }

    sizes.sort();
    sizes.dedup();
    sizes.reverse();

    sizes
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
        let device_index = self.device_index;

        unpanic(move || {
            if let Some(formats) = VIDEO_FORMATS.lock().unwrap().get(&device_path) {
                return formats.clone();
            }

            let mut formats = vec![];

            // Use nokhwa to get actual camera formats
            let camera_index = CameraIndex::Index(device_index as u32);
            match Camera::new(camera_index, RequestedFormat::new::<YuyvFormat>(RequestedFormatType::AbsoluteHighestResolution)) {
                Ok(camera) => {
                    let format = camera.camera_format();
                    let encode = convert_nokhwa_format_to_video_encode(format.format());
                    let sizes = convert_nokhwa_resolution_to_sizes(format.resolution());

                    formats.push(Format {
                        encode,
                        sizes,
                    });
                }
                Err(error) => {
                    error!("Failed to open camera device {device_path:?}: {error:?}");
                }
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

        // Use nokhwa to set camera controls
        let device_index = self.device_index;
        unpanic(move || {
            let camera_index = CameraIndex::Index(device_index as u32);
            match Camera::new(camera_index, RequestedFormat::new::<YuyvFormat>(RequestedFormatType::AbsoluteHighestResolution)) {
                Ok(mut camera) => {
                    // Map control IDs to nokhwa control names
                    let control = match control_id {
                        1 => KnownCameraControl::Brightness,
                        2 => KnownCameraControl::Contrast,
                        3 => KnownCameraControl::Saturation,
                        4 => KnownCameraControl::Hue,
                        5 => KnownCameraControl::Gain,
                        6 => KnownCameraControl::Exposure,
                        _ => return Err(std::io::Error::new(
                            std::io::ErrorKind::Unsupported,
                            format!("Control ID {control_id:?} not supported"),
                        )),
                    };

                    let control_value = ControlValueSetter::Integer(value);
                    match camera.set_camera_control(control, control_value) {
                        Ok(_) => {
                            trace!("Successfully set control {control:?} to {value:?}");
                            Ok(())
                        }
                        Err(error) => {
                            error!("Failed to set control {control:?} to {value:?}: {error:?}");
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("Failed to set control: {error:?}"),
                            ))
                        }
                    }
                }
                Err(error) => {
                    error!("Failed to open camera for control setting: {error:?}");
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Failed to open camera: {error:?}"),
                    ))
                }
            }
        })
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
        let device_index = self.device_index;
        unpanic(move || {
            let camera_index = CameraIndex::Index(device_index as u32);
            match Camera::new(camera_index, RequestedFormat::new::<YuyvFormat>(RequestedFormatType::AbsoluteHighestResolution)) {
                Ok(camera) => {
                    // Map control IDs to nokhwa control names
                    let control = match control_id {
                        1 => KnownCameraControl::Brightness,
                        2 => KnownCameraControl::Contrast,
                        3 => KnownCameraControl::Saturation,
                        4 => KnownCameraControl::Hue,
                        5 => KnownCameraControl::Gain,
                        6 => KnownCameraControl::Exposure,
                        _ => return Err(std::io::Error::new(
                            std::io::ErrorKind::Unsupported,
                            format!("Control ID {control_id:?} not supported"),
                        )),
                    };

                    match camera.camera_control(control) {
                        Ok(control_value) => {
                            trace!("Got control {control:?} value: {control_value:?}");
                            // Extract integer value from ControlValueSetter
                            match control_value.value().as_integer() {
                                Some(value) => Ok(*value),
                                None => {
                                    error!("Control {control:?} value is not an integer: {control_value:?}");
                                    Err(std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        format!("Control value is not an integer: {control_value:?}"),
                                    ))
                                }
                            }
                        }
                        Err(error) => {
                            error!("Failed to get control {control:?}: {error:?}");
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("Failed to get control: {error:?}"),
                            ))
                        }
                    }
                }
                Err(error) => {
                    error!("Failed to open camera for control reading: {error:?}");
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Failed to open camera: {error:?}"),
                    ))
                }
            }
        })
    }

    #[instrument(level = "debug")]
    fn controls(&self) -> Vec<Control> {
        // Use nokhwa to get actual camera controls
        let device_index = self.device_index;
        unpanic(move || {
            let mut controls: Vec<Control> = vec![];
            let camera_index = CameraIndex::Index(device_index as u32);
            match Camera::new(camera_index, RequestedFormat::new::<YuyvFormat>(RequestedFormatType::AbsoluteHighestResolution)) {
                Ok(camera) => {
                    // Get available controls from the camera
                    let control_mappings = vec![
                        (KnownCameraControl::Brightness, "Brightness", 1),
                        (KnownCameraControl::Contrast, "Contrast", 2),
                        (KnownCameraControl::Saturation, "Saturation", 3),
                        (KnownCameraControl::Hue, "Hue", 4),
                        (KnownCameraControl::Gain, "Gain", 5),
                        (KnownCameraControl::Exposure, "Exposure", 6),
                    ];

                    for (control, name, id) in control_mappings {
                        match camera.camera_control(control) {
                            Ok(control_value) => {
                                // Get control range if available
                                let (min, max) = match control {
                                    KnownCameraControl::Brightness => (-100, 100),
                                    KnownCameraControl::Contrast => (-100, 100),
                                    KnownCameraControl::Saturation => (-100, 100),
                                    KnownCameraControl::Hue => (-180, 180),
                                    KnownCameraControl::Gain => (0, 100),
                                    KnownCameraControl::Exposure => (-13, 0),
                                    _ => (0, 100),
                                };

                                let value = match control_value.value() {
                                    ControlValueSetter::Integer(v) => v,
                                    _ => 0,
                                };

                                controls.push(Control {
                                    name: name.to_string(),
                                    id,
                                    cpp_type: "int64".to_string(),
                                    state: ControlState {
                                        is_disabled: false,
                                        is_inactive: false,
                                    },
                                    configuration: ControlType::Slider(ControlSlider {
                                        default: 0,
                                        value,
                                        step: 1,
                                        max,
                                        min,
                                    }),
                                });
                            }
                            Err(_) => {
                                // Control not available, skip
                                trace!("Control {name:?} not available on this camera");
                            }
                        }
                    }
                }
                Err(error) => {
                    error!("Failed to open camera for control enumeration: {error:?}");
                }
            }

            controls
        })
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

        unpanic(move || {
            // Use nokhwa to enumerate actual cameras
            match nokhwa::query(nokhwa::utils::ApiBackend::Auto) {
                Ok(devices) => {
                    for (index, device) in devices.iter().enumerate() {
                        let typ = VideoSourceLocalType::from_camera_info(device);

                        let source = VideoSourceLocal {
                            name: device.human_name(),
                            device_path: format!("/dev/video{index}"),
                            typ,
                            device_index: index,
                        };
                        cameras.push(VideoSourceType::Local(source));
                    }
                }
                Err(error) => {
                    error!("Failed to enumerate cameras: {error:?}");
                }
            }

            cameras
        })
    }
}
