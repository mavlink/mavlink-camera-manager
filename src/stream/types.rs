use super::video_stream_udp::VideoStreamUdp;

pub enum StreamType {
    UDP(VideoStreamUdp),
}
