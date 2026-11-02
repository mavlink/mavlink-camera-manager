#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct Info {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInterval {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfiguration {
    Classic,
    Auto {},
    Manual {
        encoder: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        decoder: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoCaptureConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_encode: Option<String>,
    pub encode: serde_json::Value,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_configuration: Option<SourceConfiguration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CaptureConfiguration {
    Video(VideoCaptureConfiguration),
    Redirect {},
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExtendedConfiguration {
    pub thermal: bool,
    pub disable_mavlink: bool,
    pub disable_zenoh: bool,
    pub disable_thumbnails: bool,
    pub disable_lazy: bool,
}

/// Fake H265 RTSP senders cannot use the 5s lazy idle grace: `x265enc` often
/// takes longer than that to produce RTP caps, so the RTSP factory is never
/// mounted and OPTIONS stays 404.
pub const FAKE_H265_RTSP_SENDER: ExtendedConfiguration = ExtendedConfiguration {
    thermal: false,
    disable_mavlink: true,
    disable_zenoh: true,
    disable_thumbnails: false,
    disable_lazy: true,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub configuration: CaptureConfiguration,
    pub extended_configuration: Option<ExtendedConfiguration>,
}

#[derive(Debug, Deserialize)]
pub struct VideoAndStreamInformation {
    pub name: String,
    pub stream_information: StreamInformation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamStatusState {
    Running,
    Starting,
    Idle,
    Stopped,
}

#[derive(Debug, Deserialize)]
pub struct StreamStatus {
    pub id: Uuid,
    pub running: bool,
    pub state: StreamStatusState,
    pub error: Option<String>,
    pub video_and_stream: VideoAndStreamInformation,
}

#[derive(Debug, Serialize)]
pub struct PostStream {
    pub name: String,
    pub source: String,
    pub stream_information: StreamInformation,
}

#[derive(Debug, Deserialize)]
pub struct GstEncoders {
    pub encodings: BTreeMap<String, Vec<GstCodecInfo>>,
}

#[derive(Debug, Deserialize)]
pub struct GstDecoders {
    pub decodings: BTreeMap<String, Vec<GstCodecInfo>>,
}

#[derive(Debug, Deserialize)]
pub struct GstCodecInfo {
    pub name: String,
}

impl GstEncoders {
    pub fn first_factory(&self, encode: &str) -> Option<String> {
        self.encodings
            .get(encode)?
            .first()
            .map(|info| info.name.clone())
    }
}

impl GstDecoders {
    pub fn first_factory(&self, encode: &str) -> Option<String> {
        self.decodings
            .get(encode)?
            .first()
            .map(|info| info.name.clone())
    }
}
