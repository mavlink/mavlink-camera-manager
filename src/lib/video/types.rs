use mcm_api::v1::video::{Format, VideoSourceType};

use super::video_source::{VideoSource, VideoSourceFormats};

pub trait VideoSourceTypeExt {
    fn inner(&self) -> &(dyn VideoSource + '_);
}

impl VideoSourceTypeExt for VideoSourceType {
    fn inner(&self) -> &(dyn VideoSource + '_) {
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
