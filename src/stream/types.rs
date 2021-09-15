use super::stream_backend::StreamBackend;
use super::video_stream_udp::VideoStreamUdp;
use crate::video::types::CaptureConfiguration;
use crate::video_stream::types::VideoAndStreamInformation;

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use url::Url;

use paperclip::v2::{self, ResolvableApi};

//extern crate paperclip;

#[derive(Debug)]
pub enum StreamType {
    UDP(VideoStreamUdp),
}

impl StreamType {
    pub fn inner(&self) -> &(dyn StreamBackend + '_) {
        match self {
            StreamType::UDP(backend) => backend,
        }
    }

    pub fn mut_inner(&mut self) -> &mut (dyn StreamBackend + '_) {
        match self {
            StreamType::UDP(backend) => backend,
        }
    }
}

/*
impl Apiv2Schema for Url {
}*/

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
