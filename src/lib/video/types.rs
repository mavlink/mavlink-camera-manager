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

#[derive(
    Apiv2Schema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Hash,
)]
pub struct Format {
    pub encode: VideoEncodeType,
    pub sizes: Vec<Size>,
}

#[derive(
    Apiv2Schema, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Hash,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum VideoEncodeType {
    H264,
    H265,
    Mjpg,
    Nv12,
    Rgb,
    Unknown(String),
    Yuyv,
}

#[derive(
    Apiv2Schema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Hash,
)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    /// Frame intervals when [`Self::depths`] is empty (USB, fake, ONVIF).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intervals: Vec<FrameInterval>,
    /// Packed CSI depths for this size. Each depth has its own fps list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depths: Vec<SizeDepth>,
}

#[derive(
    Apiv2Schema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Hash,
)]
pub struct SizeDepth {
    pub bit_depth: u32,
    pub intervals: Vec<FrameInterval>,
}

#[derive(
    Apiv2Schema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Hash,
)]
pub struct FrameInterval {
    pub numerator: u32,
    pub denominator: u32,
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
            "NV12" => VideoEncodeType::Nv12,
            // ISP / libcamera processed RGB aliases (BGR888 is common on Pi).
            "RGB" | "RGB888" | "BGR" | "BGR888" | "RGBX" | "BGRX" | "RGBA" | "BGRA" | "XRGB"
            | "XBGR" | "ARGB" | "ABGR" | "XBGR8888" | "XRGB8888" | "RGBX8888" | "BGRX8888" => {
                VideoEncodeType::Rgb
            }
            "YUYV" | "YUY2" => VideoEncodeType::Yuyv,
            _ => VideoEncodeType::Unknown(fourcc),
        };

        Ok(res)
    }
}

impl VideoEncodeType {
    /// Parse a fourcc / pixel-format name. Unknown names become [`Self::Unknown`].
    pub fn from_fourcc(fourcc: &str) -> Self {
        match <Self as std::str::FromStr>::from_str(fourcc) {
            Ok(encode) => encode,
            Err(infallible) => match infallible {},
        }
    }
}

impl Size {
    pub fn preferred_frame_interval(&self) -> Option<FrameInterval> {
        self.intervals.first().cloned().or_else(|| {
            self.depths
                .iter()
                .find_map(|depth| depth.intervals.first().cloned())
        })
    }
}

impl FrameInterval {
    /// GStreamer stores framerate as `denominator/numerator` inverted into this type:
    /// frames per second = `denominator / numerator`.
    pub fn frames_per_second_exceeds(&self, other: &Self) -> bool {
        if self.numerator == 0 || other.numerator == 0 {
            return false;
        }
        u64::from(self.denominator) * u64::from(other.numerator)
            > u64::from(other.denominator) * u64::from(self.numerator)
    }

    pub fn frames_per_second_equals(&self, other: &Self) -> bool {
        !self.frames_per_second_exceeds(other) && !other.frames_per_second_exceeds(self)
    }
}

pub static DEFAULT_FRAME_INTERVALS: &[u32; 6] = &[60, 30, 24, 16, 10, 5];

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn from_str_maps_isp_aliases() {
        assert_eq!(
            VideoEncodeType::from_str("NV12").unwrap(),
            VideoEncodeType::Nv12
        );
        assert_eq!(
            VideoEncodeType::from_str("YUY2").unwrap(),
            VideoEncodeType::Yuyv
        );
        assert_eq!(
            VideoEncodeType::from_str("BGR888").unwrap(),
            VideoEncodeType::Rgb
        );
        assert_eq!(
            VideoEncodeType::from_str("RGB").unwrap(),
            VideoEncodeType::Rgb
        );
        assert_eq!(
            VideoEncodeType::from_str("XBGR8888").unwrap(),
            VideoEncodeType::Rgb
        );
        assert!(matches!(
            VideoEncodeType::from_str("SRGGB10_CSI2P").unwrap(),
            VideoEncodeType::Unknown(_)
        ));
    }

    #[test]
    fn frames_per_second_exceeds_compares_inverted_fractions() {
        let sixty = FrameInterval {
            numerator: 1,
            denominator: 60,
        };
        let twenty_one = FrameInterval {
            numerator: 100,
            denominator: 2119,
        };
        let twenty = FrameInterval {
            numerator: 1,
            denominator: 20,
        };
        assert!(sixty.frames_per_second_exceeds(&twenty_one));
        assert!(!twenty_one.frames_per_second_exceeds(&sixty));
        assert!(!twenty.frames_per_second_exceeds(&twenty_one));
        assert!(!twenty_one.frames_per_second_exceeds(&twenty_one));
        assert!(twenty_one.frames_per_second_exceeds(&twenty));
        assert!(sixty.frames_per_second_equals(&FrameInterval {
            numerator: 1,
            denominator: 60,
        }));
        assert!(!sixty.frames_per_second_equals(&twenty_one));
    }
}
