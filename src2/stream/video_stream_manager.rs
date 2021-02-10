use super::stream_backend::{StreamBackend, StreamType};
use super::video_stream_udp::VideoStreamUdp;

#[derive(Default)]
pub struct VideoStreamManager {
    streams: Vec<StreamType>,
}

impl VideoStreamManager {
    pub fn start(&mut self) {
        for stream in &mut self.streams {
            match stream {
                StreamType::UDP(stream) => {
                    stream.start();
                }
            }
        }
    }

    pub fn add(&mut self) {
        self.streams
            .push(StreamType::UDP(VideoStreamUdp::default()));
    }
}
