use super::video_source::VideoSource;
use super::video_source_gst::VideoSourceGst;
use super::video_source_local::VideoSourceLocal;
use super::video_source_redirect::VideoSourceRedirect;
use gst;
use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceType {
    Gst(VideoSourceGst),
    Local(VideoSourceLocal),
    Redirect(VideoSourceRedirect),
}

#[derive(Apiv2Schema, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum VideoEncodeType {
    H264,
    H265,
    Mjpg,
    Rgb,
    Unknown(String),
    Yuyv,
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

impl From<gst::Fraction> for FrameInterval {
    fn from(fraction: gst::Fraction) -> Self {
        FrameInterval {
            numerator: fraction.numer() as u32,
            denominator: fraction.denom() as u32,
        }
    }
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
    //TODO: use trait fromstr, check others places
    pub fn from_str(fourcc: &str) -> VideoEncodeType {
        let fourcc = fourcc.to_uppercase();
        match fourcc.as_str() {
            "H264" => VideoEncodeType::H264,
            "MJPG" => VideoEncodeType::Mjpg,
            "YUYV" => VideoEncodeType::Yuyv,
            _ => VideoEncodeType::Unknown(fourcc),
        }
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
