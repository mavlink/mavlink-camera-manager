use super::types::*;
use super::{stream_backend, stream_backend::StreamBackend};
use crate::mavlink::mavlink_camera::MavlinkCameraHandle;
use crate::settings;
use crate::video_stream::types::VideoAndStreamInformation;
use log::*;
use simple_error::SimpleError;
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
struct Stream {
    stream_type: StreamType,
    video_and_stream_information: VideoAndStreamInformation,
    mavlink_camera: MavlinkCameraHandle,
}

#[derive(Default)]
struct Manager {
    pub streams: Vec<Stream>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

pub fn init() {
    debug!("Starting video stream service.");
}

// Start all streams that are not running
#[allow(dead_code)]
pub fn start() {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    for stream in &mut manager.streams {
        match &mut stream.stream_type {
            StreamType::UDP(stream) => {
                stream.start();
            }
            StreamType::RTSP(stream) => {
                stream.start();
            }
            StreamType::WEBRTC(stream) => {
                stream.start();
            }
            StreamType::REDIRECT(_) => (),
        }
    }
}

pub fn streams() -> Vec<StreamStatus> {
    let manager = MANAGER.as_ref().lock().unwrap();
    let status: Vec<StreamStatus> = manager
        .streams
        .iter()
        .map(|stream| StreamStatus {
            running: stream.stream_type.inner().is_running(),
            video_and_stream: stream.video_and_stream_information.clone(),
        })
        .collect();

    return status;
}

pub fn add_stream_and_start(
    video_and_stream_information: VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    //TODO: Check if stream can handle caps
    let mut manager = MANAGER.as_ref().lock().unwrap();

    for stream in manager.streams.iter() {
        if !stream.stream_type.inner().allow_same_endpoints() {
            stream
                .video_and_stream_information
                .conflicts_with(&video_and_stream_information)?
        }
    }

    let endpoint = video_and_stream_information
        .stream_information
        .endpoints
        .first()
        .unwrap() // We have an endpoint since we have passed the point of stream creation
        .clone();

    let mut stream = stream_backend::new(&video_and_stream_information)?;
    let mavtype: mavlink::common::VideoStreamType = match &stream {
        StreamType::UDP(_) => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTPUDP,
        StreamType::RTSP(_) => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTSP,
        StreamType::REDIRECT(video_strem_redirect) => match video_strem_redirect.scheme.as_str() {
            "rtsp" => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTSP,
            "mpegts" => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_MPEG_TS_H264,
            "tcp" => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_TCP_MPEG,
            "udp" | _ => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTPUDP,
        },
        // TODO: update WEBRTC arm with the correct type once mavlink starts to support it.
        // Note: For now this is fine because most of the clients doesn't seems to be using mavtype to determine the stream type,
        // instead, they're parsing the URI's scheme itself, so as long as we pass a known scheme, it should be enough.
        StreamType::WEBRTC(_) => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTSP,
    };

    stream.mut_inner().start();
    manager.streams.push(Stream {
        stream_type: stream,
        video_and_stream_information: video_and_stream_information.clone(),
        mavlink_camera: MavlinkCameraHandle::new(
            video_and_stream_information.video_source.clone(),
            endpoint,
            mavtype,
            video_and_stream_information
                .stream_information
                .extended_configuration
                .unwrap_or_default()
                .thermal,
        ),
    });

    //TODO: Create function to update settings
    let video_and_stream_informations = manager
        .streams
        .iter()
        .map(|stream| stream.video_and_stream_information.clone())
        .collect();
    settings::manager::set_streams(&video_and_stream_informations);
    return Ok(());
}

pub fn remove_stream(stream_name: &str) -> Result<(), SimpleError> {
    let find_stream = |stream: &Stream| stream.video_and_stream_information.name == *stream_name;

    let mut manager = MANAGER.as_ref().lock().unwrap();
    match manager.streams.iter().position(find_stream) {
        Some(index) => {
            manager.streams.remove(index);
            let video_and_stream_informations = manager
                .streams
                .iter()
                .map(|stream| stream.video_and_stream_information.clone())
                .collect();
            settings::manager::set_streams(&video_and_stream_informations);
            Ok(())
        }
        None => Err(SimpleError::new(
            "Identification does not match any stream.",
        )),
    }
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
