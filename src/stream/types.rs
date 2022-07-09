use super::{
    stream_backend::StreamBackend, video_stream_redirect::VideoStreamRedirect,
    video_stream_rtsp::VideoStreamRtsp, video_stream_tcp::VideoStreamTcp,
    video_stream_udp::VideoStreamUdp, video_stream_webrtc::VideoStreamWebRTC,
};
use crate::{
    video::types::{FrameInterval, VideoEncodeType},
    video_stream::types::VideoAndStreamInformation,
};

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug)]
#[allow(dead_code)]
pub enum StreamType {
    UDP(VideoStreamUdp),
    TCP(VideoStreamTcp),
    RTSP(VideoStreamRtsp),
    REDIRECT(VideoStreamRedirect),
    WEBRTC(VideoStreamWebRTC),
}

impl StreamType {
    pub fn inner(&self) -> &(dyn StreamBackend + '_) {
        match self {
            StreamType::UDP(backend) => backend,
            StreamType::TCP(backend) => backend,
            StreamType::RTSP(backend) => backend,
            StreamType::REDIRECT(backend) => backend,
            StreamType::WEBRTC(backend) => backend,
        }
    }

    pub fn mut_inner(&mut self) -> &mut (dyn StreamBackend + '_) {
        match self {
            StreamType::UDP(backend) => backend,
            StreamType::TCP(backend) => backend,
            StreamType::RTSP(backend) => backend,
            StreamType::REDIRECT(backend) => backend,
            StreamType::WEBRTC(backend) => backend,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VideoCaptureConfiguration {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct RedirectCaptureConfiguration {}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CaptureConfiguration {
    VIDEO(VideoCaptureConfiguration),
    REDIRECT(RedirectCaptureConfiguration),
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ExtendedConfiguration {
    pub thermal: bool,
}

impl Default for ExtendedConfiguration {
    fn default() -> Self {
        Self { thermal: false }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Apiv2Schema)]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub configuration: CaptureConfiguration,
    pub extended_configuration: Option<ExtendedConfiguration>,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct StreamStatus {
    pub running: bool,
    pub video_and_stream: VideoAndStreamInformation,
}
