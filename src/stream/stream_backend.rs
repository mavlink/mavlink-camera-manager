use super::video_stream_udp::VideoStreamUdp;

//TODO: Move to types.rs
pub enum StreamType {
    //TODO: Maybe endpoint should be inside the stream definition
    UDP(VideoStreamUdp),
}

pub trait StreamBackend {
    fn start(&mut self) -> bool;
    fn stop(&mut self) -> bool;
    fn restart(&mut self);
    fn set_pipeline_description(&mut self, description: &'static str);
}
