use super::video_stream_udp::VideoStreamUdp;
use crate::video::types::FrameSize;
use url::Url;

pub enum StreamType {
    UDP(VideoStreamUdp),
}

pub struct StreamInformation {
    pub stream_type: StreamType,
    pub endpoints: Vec<Url>,
    pub frame_size: FrameSize,
}
