use super::video_source::VideoSource;
use super::video_source_usb::VideoSourceUsb;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub enum VideoSourceType {
    Usb(VideoSourceUsb),
}

#[derive(Debug, Serialize)]
pub enum VideoEncodeType {
    UNKNOWN(String),
    H264,
    MJPG,
    YUYV,
}

#[derive(Debug, Serialize)]
pub struct FrameSize {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Control {
    pub name: String,
    pub cpp_type: String,
    pub id: u64,
    pub configuration: ControlType,
}

#[derive(Clone, Debug, Serialize)]
pub enum ControlType {
    Bool(ControlBool),
    Slider(ControlSlider),
    Menu(ControlMenu),
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlBool {
    pub default: i32,
    pub value: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlSlider {
    pub default: i32,
    pub value: i64,
    pub step: i32,
    pub max: i32,
    pub min: i32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlMenu {
    pub default: i32,
    pub value: i64,
    pub options: Vec<ControlOption>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlOption {
    pub name: String,
    pub value: i64,
}

impl VideoSourceType {
    pub fn inner(&self) -> impl VideoSource {
        match self {
            VideoSourceType::Usb(source) => (*source).clone(),
            _ => unreachable!(),
        }
    }
}

impl VideoEncodeType {
    pub fn from_str(fourcc: &str) -> VideoEncodeType {
        return match fourcc {
            "H264" => VideoEncodeType::H264,
            "MJPG" => VideoEncodeType::MJPG,
            "YUYV" => VideoEncodeType::YUYV,
            _ => VideoEncodeType::UNKNOWN(fourcc.to_string()),
        };
    }
}

impl Default for ControlType {
    fn default() -> Self {
        ControlType::Bool(ControlBool {
            default: 0,
            value: 0,
        })
    }
}
