use super::stream_backend::{StreamBackend, StreamType};
use super::video_stream_udp::VideoStreamUdp;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Manager {
    pub streams: Vec<StreamType>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

pub fn init() {
    //TODO: Remove test videos
    //add("videotestsrc pattern=snow ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5600");
    add("videotestsrc pattern=ball ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5601");
}

pub fn start() {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    for stream in &mut manager.streams {
        match stream {
            StreamType::UDP(stream) => {
                stream.start();
            }
        }
    }
}

//TODO: rework to use UML definition
pub fn add(description: &'static str) {
    let mut stream = VideoStreamUdp::default();
    stream.set_pipeline_description(description);
    let mut manager = MANAGER.as_ref().lock().unwrap();
    manager.streams.push(StreamType::UDP(stream));
}
