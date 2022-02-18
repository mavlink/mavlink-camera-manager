use super::types::*;
use super::video_source::{VideoSource, VideoSourceAvailable};
use super::video_source_local::VideoSourceLocal;

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceGstType {
    // TODO: local should have a pipeline also
    Local(VideoSourceLocal),
    Fake(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSourceGst {
    pub name: String,
    pub source: VideoSourceGstType,
}

impl VideoSource for VideoSourceGst {
    fn name(&self) -> &String {
        return &self.name;
    }

    fn source_string(&self) -> &str {
        match &self.source {
            VideoSourceGstType::Local(local) => &local.source_string(),
            VideoSourceGstType::Fake(string) => &string,
        }
    }

    fn formats(&self) -> Vec<Format> {
        match &self.source {
            VideoSourceGstType::Local(local) => local.formats(),
            VideoSourceGstType::Fake(_) => {
                let intervals: Vec<FrameInterval> = [60, 30, 24, 16, 10, 5]
                    .iter()
                    .map(|&frame_interval| FrameInterval {
                        denominator: frame_interval,
                        numerator: 1,
                    })
                    .collect();

                let sizes = [
                    (320, 240),
                    (640, 480),
                    (720, 480),
                    (960, 720),
                    (1280, 720),
                    (1280, 1080),
                    (1440, 1080),
                    (1920, 1080),
                ]
                .iter()
                .map(|&(width, height)| Size {
                    width,
                    height,
                    intervals: intervals.clone(),
                })
                .collect();

                vec![Format {
                    encode: VideoEncodeType::H264,
                    sizes,
                }]
            }
        }
    }

    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn set_control_by_id(&self, _control_id: u64, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn control_value_by_id(&self, _control_id: u64) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn controls(&self) -> Vec<Control> {
        vec![]
    }

    fn is_valid(&self) -> bool {
        match &self.source {
            VideoSourceGstType::Local(local) => local.is_valid(),
            VideoSourceGstType::Fake(string) => match string.as_str() {
                // All valid members are from: https://gstreamer.freedesktop.org/documentation/videotestsrc/index.html?gi-language=c#members-2
                "ball" | "bar" | "black" | "blink" | "blue" | "chroma" | "circular" | "gamut"
                | "gradient" | "green" | "pinwheel" | "red" | "smpte" | "smpte100" | "smpte75"
                | "snow" | "solid" | "spokes" | "white" | "zone" => true,
                _ => false,
            },
        }
    }

    fn is_shareable(&self) -> bool {
        return true;
    }
}

impl VideoSourceAvailable for VideoSourceGst {
    fn cameras_available() -> Vec<VideoSourceType> {
        vec![VideoSourceType::Gst(VideoSourceGst {
            name: "Fake source".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        })]
    }
}
