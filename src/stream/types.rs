use super::stream_backend::StreamBackend;
use super::video_stream_rtsp::VideoStreamRtsp;
use super::video_stream_udp::VideoStreamUdp;
use crate::{
    video::types::{FrameInterval, VideoEncodeType},
    video_stream::types::VideoAndStreamInformation,
};

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug)]
pub enum StreamType {
    UDP(VideoStreamUdp),
    RTSP(VideoStreamRtsp),
}

impl StreamType {
    pub fn inner(&self) -> &(dyn StreamBackend + '_) {
        match self {
            StreamType::UDP(backend) => backend,
            StreamType::RTSP(backend) => backend,
        }
    }

    pub fn mut_inner(&mut self) -> &mut (dyn StreamBackend + '_) {
        match self {
            StreamType::UDP(backend) => backend,
            StreamType::RTSP(backend) => backend,
        }
    }
}

/*
impl Apiv2Schema for Url {
}*/

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct CaptureConfiguration {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Apiv2Schema)]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub configuration: CaptureConfiguration,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct StreamStatus {
    pub running: bool,
    pub video_and_stream: VideoAndStreamInformation,
}
