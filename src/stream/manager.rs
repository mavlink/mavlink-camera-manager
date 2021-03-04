use super::types::*;
use super::video_stream_udp::VideoStreamUdp;
use super::{stream_backend, stream_backend::StreamBackend};
use crate::video::{
    types::{VideoEncodeType, VideoSourceType},
    video_source,
};
use crate::video_stream::types::VideoAndStreamInformation;
use log::*;
use simple_error::SimpleError;
use std::sync::{Arc, Mutex};
use url::Url;

#[derive(Default)]
struct Manager {
    pub streams: Vec<(StreamType, VideoAndStreamInformation)>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

pub fn init() {
    debug!("Starting video stream service.");
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
        .map(|(stream, video_and_stream)| StreamStatus {
            running: stream.inner().is_running(),
            video_and_stream: video_and_stream.clone(),
        })
        .collect();

    return status;
}

pub fn add_stream_and_start(
    video_and_stream_information: VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    let mut stream = stream_backend::create_stream(&video_and_stream_information)?;
    stream.mut_inner().start();
    manager.streams.push((stream, video_and_stream_information));
    return Ok(());
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
