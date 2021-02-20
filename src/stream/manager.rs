use super::stream_backend::StreamBackend;
use super::types::StreamType;
use super::video_stream_udp::VideoStreamUdp;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Manager {
    pub streams: Vec<StreamType>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

// Init stream manager, should be done inside main
pub fn init() {
    add("videotestsrc pattern=ball ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5601");
}

// Start all streams that are not running
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
// Add a new pipeline string to run
pub fn add(description: &'static str) {
    let mut stream = VideoStreamUdp::default();
    stream.set_pipeline_description(description);
    let mut manager = MANAGER.as_ref().lock().unwrap();
    manager.streams.push(StreamType::UDP(stream));
}
