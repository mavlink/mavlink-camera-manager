use super::video_stream_udp::VideoStreamUdp;
use crate::video::types::{FrameSize, VideoEncodeType};
use url::Url;

#[derive(Debug)]
pub enum StreamType {
    UDP(VideoStreamUdp),
}

#[derive(Debug)]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub frame_size: FrameSize,
}
