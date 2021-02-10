use super::stream_backend::{StreamBackend, StreamType};
use super::video_stream_udp::VideoStreamUdp;

#[derive(Default)]
pub struct VideoStreamManager {
    streams: Vec<StreamType>,
}

impl VideoStreamManager {
    pub fn _init(&mut self) {
        self.add("videotestsrc pattern=snow ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5600");
        self.add("videotestsrc pattern=ball ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5601");
    }

    pub fn start(&mut self) {
        for stream in &mut self.streams {
            match stream {
                StreamType::UDP(stream) => {
                    stream.start();
                }
            }
        }
    }

    //TODO: rework to use UML definition
    pub fn add(&mut self, description: &'static str) {
        let mut stream = VideoStreamUdp::default();
        stream.set_pipeline_description(description);
        self.streams.push(StreamType::UDP(stream));
    }
}
