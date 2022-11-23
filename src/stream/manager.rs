use std::sync::{Arc, Mutex};

use crate::settings;
use crate::video::types::VideoSourceType;
use crate::video_stream::types::VideoAndStreamInformation;

use anyhow::Result;
use simple_error::{simple_error, SimpleResult};

use tracing::*;

use super::{
    pipeline::pipeline::{Pipeline, PipelineGstreamerInterface},
    stream::Stream,
    types::StreamStatus,
};

#[derive(Default)]
struct Manager {
    pub streams: Vec<Stream>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

impl Manager {
    fn update_settings(&self) {
        let video_and_stream_informations = self
            .streams
            .iter()
            .map(|stream| stream.video_and_stream_information.clone())
            .collect();

        settings::manager::set_streams(&video_and_stream_informations);
    }
}

pub fn init() {
    debug!("Starting video stream service.");

    if let Err(error) = gstreamer::init() {
        error!("Error! {error}");
    };

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

pub fn streams() -> Vec<StreamStatus> {
    let manager = MANAGER.as_ref().lock().unwrap();

    manager
        .streams
        .iter()
        .map(|stream| StreamStatus {
            running: match &stream.pipeline {
                Pipeline::V4l(pipeline) => pipeline.is_running(),
                Pipeline::Fake(pipeline) => pipeline.is_running(),
                Pipeline::Redirect(pipeline) => pipeline.is_running(),
            },
            video_and_stream: stream.video_and_stream_information.clone(),
        })
        .collect()
}

pub fn add_stream_and_start(video_and_stream_information: VideoAndStreamInformation) -> Result<()> {
    let stream = Stream::try_new(&video_and_stream_information)?;

    let mut manager = MANAGER.as_ref().lock().unwrap();
    for stream in manager.streams.iter() {
        stream
            .video_and_stream_information
            .conflicts_with(&video_and_stream_information)?
    }
    manager.streams.push(stream);

    manager.update_settings();
    return Ok(());
}

pub fn remove_stream(stream_name: &str) -> SimpleResult<()> {
    let find_stream = |stream: &Stream| stream.video_and_stream_information.name == *stream_name;

    let mut manager = MANAGER.as_ref().lock().unwrap();
    match manager.streams.iter().position(find_stream) {
        Some(index) => {
            manager.streams.remove(index);
            manager.update_settings();
            Ok(())
        }
        None => Err(simple_error!("Identification does not match any stream.")),
    }
}
