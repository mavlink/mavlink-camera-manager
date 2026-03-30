use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "UPPERCASE")]
#[non_exhaustive]
pub enum VideoEncodeType {
    H264,
    H265,
    Mjpg,
    Rgb,
    Unknown(String),
    Yuyv,
}

impl std::fmt::Display for VideoEncodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::H264 => write!(f, "H264"),
            Self::H265 => write!(f, "H265"),
            Self::Mjpg => write!(f, "MJPG"),
            Self::Rgb => write!(f, "RGB"),
            Self::Unknown(s) => write!(f, "{s}"),
            Self::Yuyv => write!(f, "YUYV"),
        }
    }
}

impl std::str::FromStr for VideoEncodeType {
    type Err = std::convert::Infallible;

    fn from_str(fourcc: &str) -> Result<Self, Self::Err> {
        let fourcc = fourcc.to_uppercase();
        let res = match fourcc.as_str() {
            "H264" => VideoEncodeType::H264,
            "H265" | "HEVC" => VideoEncodeType::H265,
            "MJPG" => VideoEncodeType::Mjpg,
            "YUYV" => VideoEncodeType::Yuyv,
            _ => VideoEncodeType::Unknown(fourcc),
        };

        Ok(res)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct Format {
    pub encode: VideoEncodeType,
    pub sizes: Vec<Size>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub intervals: Vec<FrameInterval>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct FrameInterval {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub enum VideoSourceType {
    Gst(VideoSourceGst),
    Local(VideoSourceLocal),
    Onvif(VideoSourceOnvif),
    Redirect(VideoSourceRedirect),
}

impl std::fmt::Display for VideoSourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gst(_) => write!(f, "Gst"),
            Self::Local(_) => write!(f, "Local"),
            Self::Onvif(_) => write!(f, "Onvif"),
            Self::Redirect(_) => write!(f, "Redirect"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub enum VideoSourceLocalType {
    Unknown(String),
    Usb(String),
    LegacyRpiCam(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct VideoSourceLocal {
    pub name: String,
    pub device_path: String,
    #[serde(rename = "type")]
    pub typ: VideoSourceLocalType,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub enum VideoSourceGstType {
    Local(VideoSourceLocal),
    Fake(String),
    QR(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct VideoSourceGst {
    pub name: String,
    pub source: VideoSourceGstType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub enum VideoSourceOnvifType {
    Onvif(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct VideoSourceOnvif {
    pub name: String,
    pub source: VideoSourceOnvifType,
    #[serde(flatten)]
    pub device_information: OnvifDeviceInformation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct OnvifDeviceInformation {
    pub manufacturer: String,
    pub model: String,
    pub firmware_version: String,
    pub serial_number: String,
    pub hardware_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub enum VideoSourceRedirectType {
    Redirect(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct VideoSourceRedirect {
    pub name: String,
    pub source: VideoSourceRedirectType,
}
