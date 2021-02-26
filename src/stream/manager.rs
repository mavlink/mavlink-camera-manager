use super::types::*;
use super::video_stream_udp::VideoStreamUdp;
use super::{stream_backend, stream_backend::StreamBackend};
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video_stream::types::VideoAndStreamInformation;
use std::sync::{Arc, Mutex};
use url::Url;

#[derive(Default)]
struct Manager {
    pub streams: Vec<(StreamType, VideoAndStreamInformation)>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

// Init stream manager, should be done inside main
pub fn init() {
    use crate::video::{
        types::{CaptureConfiguration, FrameInterval},
        video_source_local::{VideoSourceLocal, VideoSourceLocalType},
    };

    let video_and_stream_information = VideoAndStreamInformation {
        name: "Test".into(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse("udp://0.0.0.0:5601").unwrap()],
            configuration: CaptureConfiguration {
                encode: VideoEncodeType::H264,
                height: 1080,
                width: 1920,
                frame_interval: FrameInterval {
                    numerator: 1,
                    denominator: 30,
                },
            },
        },
        video_source: VideoSourceType::Local(VideoSourceLocal {
            name: "PotatoCam".into(),
            device_path: "/dev/video0".into(),
            typ: VideoSourceLocalType::Unknown("TestPotatoCam".into()),
        }),
    };

    let stream = stream_backend::create_stream(&video_and_stream_information).unwrap();

    let mut manager = MANAGER.as_ref().lock().unwrap();
    manager.streams.push((stream, video_and_stream_information));
}

// Start all streams that are not running
pub fn start() {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    for (stream, _) in &mut manager.streams {
        match stream {
            StreamType::UDP(stream) => {
                stream.start();
            }
        }
    }
}

pub fn streams() -> Vec<StreamStatus> {
    let manager = MANAGER.as_ref().lock().unwrap();
    let status: Vec<StreamStatus> = manager
        .streams
        .iter()
        .map(|(stream, information)| StreamStatus {
            running: stream.inner().is_running(),
            information: information.clone(),
        })
        .collect();

    return status;
}

//TODO: rework to use UML definition
// Add a new pipeline string to run
/*
pub fn add(description: &'static str) {
    let mut stream = VideoStreamUdp::default();
    stream.set_pipeline_description(description);
    let mut manager = MANAGER.as_ref().lock().unwrap();
    manager.streams.push(StreamType::UDP(stream));
}
*/
