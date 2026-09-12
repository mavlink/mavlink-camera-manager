#![allow(dead_code)]

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
pub struct VideoCaptureConfiguration {
    pub encode: serde_json::Value,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
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
