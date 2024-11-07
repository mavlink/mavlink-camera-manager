use gst;
use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

use super::{
    video_source::{VideoSource, VideoSourceFormats},
    video_source_gst::VideoSourceGst,
    video_source_local::VideoSourceLocal,
    video_source_onvif::VideoSourceOnvif,
    video_source_redirect::VideoSourceRedirect,
};

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceType {
    Gst(VideoSourceGst),
    Local(VideoSourceLocal),
    Onvif(VideoSourceOnvif),
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
            VideoSourceType::Onvif(onvif) => onvif,
            VideoSourceType::Redirect(redirect) => redirect,
        }
    }
}

impl VideoSourceFormats for VideoSourceType {
    async fn formats(&self) -> Vec<Format> {
        match self {
            VideoSourceType::Gst(gst) => gst.formats().await,
            VideoSourceType::Local(local) => local.formats().await,
            VideoSourceType::Onvif(onvif) => onvif.formats().await,
            VideoSourceType::Redirect(redirect) => redirect.formats().await,
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
