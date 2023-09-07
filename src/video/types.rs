use super::video_source::VideoSource;
use super::video_source_gst::VideoSourceGst;
use super::video_source_local::VideoSourceLocal;
use super::video_source_redirect::VideoSourceRedirect;
use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

pub const KNOWN_RTP_RAW_FORMATS: [&str; 9] = [
    "RGB", "RGBA", "BGR", "BGRA", "AYUV", "UYVY", "I420", "Y41B", "UYVP",
];
pub const DEFAULT_RAW_FORMAT: &str = "I420";

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceType {
    Gst(VideoSourceGst),
    Local(VideoSourceLocal),
    Redirect(VideoSourceRedirect),
}

#[derive(Apiv2Schema, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum VideoEncodeType {
    H265,
    H264,
    Mjpg,
    #[serde(untagged)]
    Raw(String),
    #[serde(untagged)]
    Unknown(String),
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct Format {
    pub encode: VideoEncodeType,
    pub sizes: Vec<Size>,
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub intervals: Vec<FrameInterval>,
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct FrameInterval {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Apiv2Schema, Clone, Debug, Default, Serialize)]
pub struct Control {
    pub name: String,
    pub cpp_type: String,
    pub id: u64,
    pub state: ControlState,
    pub configuration: ControlType,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub enum ControlType {
    Bool(ControlBool),
    Slider(ControlSlider),
    Menu(ControlMenu),
}

#[derive(Apiv2Schema, Clone, Debug, Default, Serialize)]
pub struct ControlState {
    pub is_disabled: bool,
    pub is_inactive: bool,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlBool {
    pub default: i32,
    pub value: i64,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlSlider {
    pub default: i32,
    pub value: i64,
    pub step: i32,
    pub max: i32,
    pub min: i32,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlMenu {
    pub default: i32,
    pub value: i64,
    pub options: Vec<ControlOption>,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlOption {
    pub name: String,
    pub value: i64,
}

impl VideoSourceType {
    pub fn inner(&self) -> &(dyn VideoSource + '_) {
        match self {
            VideoSourceType::Local(local) => local,
            VideoSourceType::Gst(gst) => gst,
            VideoSourceType::Redirect(redirect) => redirect,
        }
    }
}

impl VideoEncodeType {
    pub fn from_str(fourcc: &str) -> VideoEncodeType {
        let fourcc = fourcc.to_uppercase();

        let fourcc = match fourcc.as_str() {
            "H264" => return VideoEncodeType::H264,
            "MJPG" => return VideoEncodeType::Mjpg,
            // TODO: We can implement a [GstDeviceMonitor](https://gstreamer.freedesktop.org/documentation/gstreamer/gstdevicemonitor.html?gi-language=c) and then this manual mapping between v4l's and gst's fourcc will not be neccessary anymore.
            // A list of possible v4l fourcc from the Linux docs:
            // - [YUV](https://www.kernel.org/doc/html/v4.8/media/uapi/v4l/yuv-formats.html)
            // - [RGB](https://www.kernel.org/doc/html/v4.8/media/uapi/v4l/pixfmt-rgb.html)
            // - [compressed](https://www.kernel.org/doc/html/v4.8/media/uapi/v4l/pixfmt-013.html)
            "YUYV" => "YUY2".to_string(),
            _ => fourcc,
        };

        // Use Gstreamer to recognize raw formats. [GStreamer raw formats](https://gstreamer.freedesktop.org/documentation/additional/design/mediatype-video-raw.html?gi-language=c#formats)
        use std::ffi::CString;
        if let Ok(c_char_array) = CString::new(fourcc.clone()).map(|c_str| c_str.into_raw()) {
            use gst_video::ffi::*;

            let gst_video_format = unsafe { gst_video_format_from_string(c_char_array) };

            if !matches!(
                gst_video_format,
                GST_VIDEO_FORMAT_UNKNOWN | GST_VIDEO_FORMAT_ENCODED
            ) {
                return VideoEncodeType::Raw(fourcc);
            }
        }

        VideoEncodeType::Unknown(fourcc)
    }
}
impl<'de> Deserialize<'de> for VideoEncodeType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct VideoEncodeTypeVisitor;

        use std::fmt;

        impl<'de> serde::de::Visitor<'de> for VideoEncodeTypeVisitor {
            type Value = VideoEncodeType;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("valid video encode type")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Self::Value::from_str(value))
            }
        }

        deserializer.deserialize_str(VideoEncodeTypeVisitor)
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

pub static STANDARD_SIZES: &[(u32, u32); 16] = &[
    (7680, 4320),
    (7200, 3060),
    (3840, 2160),
    (2560, 1440),
    (1920, 1080),
    (1600, 1200),
    (1440, 1080),
    (1280, 1080),
    (1280, 720),
    (1024, 768),
    (960, 720),
    (800, 600),
    (640, 480),
    (640, 360),
    (320, 240),
    (256, 144),
];

mod tests {

    #[test]
    fn test_video_encode_type_serde() {
        use super::VideoEncodeType;

        for (encode, encode_str) in [
            (VideoEncodeType::Mjpg, "\"MJPG\""),
            (VideoEncodeType::H264, "\"H264\""),
            (VideoEncodeType::Raw("I420".to_string()), "\"I420\""),
            (VideoEncodeType::Unknown("POTATO".to_string()), "\"POTATO\""),
        ] {
            debug_assert_eq!(encode_str, serde_json::to_string(&encode).unwrap());

            debug_assert_eq!(encode, serde_json::from_str(encode_str).unwrap());
        }

        // Ensure UYUV is parsed as YUY2:
        let encode = VideoEncodeType::Raw("YUY2".to_string());
        let legacy_encode_str = "\"YUYV\"";
        debug_assert_eq!(encode, serde_json::from_str(legacy_encode_str).unwrap());
    }
}
