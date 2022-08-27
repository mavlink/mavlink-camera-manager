use super::types::*;
use super::{stream_backend, stream_backend::StreamBackend};
use crate::mavlink::mavlink_camera::MavlinkCameraHandle;
use crate::settings;
use crate::video::types::VideoSourceType;
use crate::video_stream::types::VideoAndStreamInformation;
use simple_error::{simple_error, SimpleResult};
use std::sync::{Arc, Mutex};
use tracing::*;

#[allow(dead_code)]
struct Stream {
    stream_type: StreamType,
    video_and_stream_information: VideoAndStreamInformation,
    mavlink_camera: Option<MavlinkCameraHandle>,
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

    config_gstreamer_plugins();
}

fn config_gstreamer_plugins() {
    let plugins_config = crate::cli::manager::gst_feature_rank();

    for config in plugins_config {
        match crate::stream::gst::utils::set_plugin_rank(config.name.as_str(), config.rank) {
            Ok(_) => info!(
                "Gstreamer Plugin {name:#?} configured with rank {rank:#?}.",
                name = config.name,
                rank = config.rank,
            ),
            Err(error) => error!("Error when trying to configure plugin {name:#?} rank to {rank:#?}. Reason: {error}", name = config.name, rank = config.rank, error=error.to_string()),
        }
    }
}

pub fn start_default() {
    MANAGER.as_ref().lock().unwrap().streams.clear();

    let mut streams = settings::manager::streams();

    // Update all local video sources to make sure that is available
    streams.iter_mut().for_each(|stream| {
        if let VideoSourceType::Local(source) = &mut stream.video_source {
            if !source.update_device() {
                error!("Source appears to be invalid or not found: {source:#?}");
            }
        }
    });

    // Remove all invalid video_sources
    let streams: Vec<VideoAndStreamInformation> = streams
        .into_iter()
        .filter(|stream| stream.video_source.inner().is_valid())
        .collect();

    debug!("streams: {streams:#?}");

    for stream in streams {
        add_stream_and_start(stream).unwrap_or_else(|error| {
            error!("Not possible to start stream: {error}");
        });
    }
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
) -> SimpleResult<()> {
    //TODO: Check if stream can handle caps
    let mut manager = MANAGER.as_ref().lock().unwrap();

    for stream in manager.streams.iter() {
        if !stream.stream_type.inner().allow_same_endpoints() {
            stream
                .video_and_stream_information
                .conflicts_with(&video_and_stream_information)?
        }
    }

    let mut stream = stream_backend::new(&video_and_stream_information)?;

    let mavlink_camera = MavlinkCameraHandle::try_new(&video_and_stream_information, &stream);

    stream.mut_inner().start();
    manager.streams.push(Stream {
        stream_type: stream,
        video_and_stream_information: video_and_stream_information.clone(),
        mavlink_camera,
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

pub fn remove_stream(stream_name: &str) -> SimpleResult<()> {
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
        None => Err(simple_error!("Identification does not match any stream.")),
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
