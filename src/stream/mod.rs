pub mod gst;
pub mod manager;
pub mod pipeline;
pub mod rtsp;
pub mod sink;
pub mod types;
pub mod webrtc;

use std::sync::{Arc, Mutex};

use crate::mavlink::mavlink_camera::MavlinkCameraHandle;
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video::video_source::cameras_available;
use crate::video_stream::types::VideoAndStreamInformation;

use manager::Manager;
use pipeline::Pipeline;
use sink::{create_image_sink, create_rtsp_sink, create_udp_sink};
use types::*;
use webrtc::signalling_protocol::PeerId;
use webrtc::signalling_server::StreamManagementInterface;

use anyhow::{anyhow, Context, Result};

use tracing::*;

use self::gst::utils::wait_for_element_state;
use self::rtsp::rtsp_server::RTSP_SERVER_PORT;
use self::sink::SinkInterface;

use ::gst::prelude::*;

#[derive(Debug)]
pub struct Stream {
    state: Arc<Mutex<StreamState>>,
    terminated: Arc<Mutex<bool>>,
    _watcher_thread_handle: std::thread::JoinHandle<()>,
}

#[derive(Debug)]
pub struct StreamState {
    pub pipeline_id: PeerId,
    pub pipeline: Pipeline,
    pub video_and_stream_information: VideoAndStreamInformation,
    pub mavlink_camera: Option<MavlinkCameraHandle>,
}

impl Stream {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline_id = Manager::generate_uuid();

        let state = Arc::new(Mutex::new(StreamState::try_new(
            video_and_stream_information,
            &pipeline_id,
        )?));

        let terminated = Arc::new(Mutex::new(false));
        let terminated_cloned = terminated.clone();

        let video_and_stream_information_cloned = video_and_stream_information.clone();
        let state_cloned = state.clone();
        let _watcher_thread_handle = std::thread::Builder::new()
            .name(format!("Stream-{pipeline_id}"))
            .spawn(move || {
                Self::watcher(
                    video_and_stream_information_cloned,
                    pipeline_id,
                    state_cloned,
                    terminated_cloned,
                )
            })
            .context(format!(
                "Failed when spawing PipelineRunner thread for Pipeline {pipeline_id:#?}"
            ))?;

        Ok(Self {
            state,
            terminated,
            _watcher_thread_handle,
        })
    }

    #[instrument(level = "debug")]
    fn watcher(
        video_and_stream_information: VideoAndStreamInformation,
        pipeline_id: uuid::Uuid,
        state: Arc<Mutex<StreamState>>,
        terminated: Arc<Mutex<bool>>,
    ) {
        // To reduce log size, each report we raise the report interval geometrically until a maximum value is reached:
        let report_interval_mult = 2;
        let report_interval_max = 60;
        let mut report_interval = std::time::Duration::from_secs(1);
        let mut last_report_time = std::time::Instant::now();

        let mut video_and_stream_information = video_and_stream_information;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));

            if !state
                .lock()
                .unwrap()
                .pipeline
                .inner_state_as_ref()
                .pipeline_runner
                .is_running()
            {
                // If it's a camera, try to update the device
                if let VideoSourceType::Local(_) = video_and_stream_information.video_source {
                    let mut streams = vec![video_and_stream_information.clone()];
                    let mut candidates = cameras_available();

                    // Discards any source from other running streams, otherwise we'd be trying to create a stream from a device in use (which is not possible)
                    let current_running_streams = manager::streams()
                        .unwrap()
                        .iter()
                        .filter_map(|status| {
                            status
                                .running
                                .then_some(status.video_and_stream.video_source.clone())
                        })
                        .collect::<Vec<VideoSourceType>>();
                    candidates.retain(|candidate| !current_running_streams.contains(candidate));

                    let should_report =
                        std::time::Instant::now() - last_report_time >= report_interval;

                    // Find the best candidate
                    manager::update_devices(&mut streams, &mut candidates, should_report);
                    video_and_stream_information = streams.first().unwrap().clone();

                    // Check if the chosen video source is available
                    match crate::video::video_source::get_video_source(
                        video_and_stream_information
                            .video_source
                            .inner()
                            .source_string(),
                    ) {
                        Ok(best_candidate) => {
                            video_and_stream_information.video_source = best_candidate;
                        }
                        Err(error) => {
                            if should_report {
                                warn!("Failed to recreate the stream {pipeline_id:?}: {error:?}. Is the device connected? Trying again each second until the success or stream is removed. Next report in {report_interval:?} to reduce log size.");
                                last_report_time = std::time::Instant::now();
                                report_interval *= report_interval_mult;
                                if report_interval
                                    > std::time::Duration::from_secs(report_interval_max)
                                {
                                    report_interval =
                                        std::time::Duration::from_secs(report_interval_max);
                                }
                            }

                            std::thread::sleep(std::time::Duration::from_secs(1));
                            continue;
                        }
                    }
                }

                // Try to recreate the stream
                if let Ok(mut state) = state.lock() {
                    *state = match StreamState::try_new(&video_and_stream_information, &pipeline_id)
                    {
                        Ok(state) => state,
                        Err(error) => {
                            error!("Failed to recreate the stream {pipeline_id:?}: {error:#?}. Trying again in one second...");
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            continue;
                        }
                    };
                }
            }

            if *terminated.lock().unwrap() {
                debug!("Ending stream {pipeline_id:?}.");
                break;
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        *self.terminated.lock().unwrap() = true;
    }
}

impl StreamState {
    #[instrument(level = "debug")]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        pipeline_id: &uuid::Uuid,
    ) -> Result<Self> {
        if let Err(error) = validate_endpoints(video_and_stream_information) {
            return Err(anyhow!("Failed validating endpoints. Reason: {error:?}"));
        }

        let pipeline = Pipeline::try_new(video_and_stream_information, pipeline_id)?;

        // Only create the Mavlink Handle when mavlink is not disabled
        let mavlink_camera = match video_and_stream_information
            .stream_information
            .extended_configuration
        {
            Some(ExtendedConfiguration {
                thermal: _,
                disable_mavlink: true,
            }) => None,
            _ => MavlinkCameraHandle::try_new(video_and_stream_information).ok(),
        };

        let mut stream = StreamState {
            pipeline_id: *pipeline_id,
            pipeline,
            video_and_stream_information: video_and_stream_information.clone(),
            mavlink_camera,
        };

        match &video_and_stream_information.video_source {
            VideoSourceType::Redirect(_) => return Ok(stream), // Do not add any Sink if it's a redirect Pipeline
            VideoSourceType::Gst(_) | VideoSourceType::Local(_) => (),
        }

        let endpoints = &video_and_stream_information.stream_information.endpoints;

        // Disable concurrent RTSP and UDP sinks creation, as it is failing.
        if endpoints.iter().any(|endpoint| endpoint.scheme() == "udp")
            && endpoints.iter().any(|endpoint| endpoint.scheme() == "rtsp")
        {
            return Err(anyhow!(
                "UDP endpoints won't work together with RTSP endpoints. You need to choose one. This is a (temporary) software limitation, if this is a feature you need, please, contact us."
            ));
        }

        if endpoints.iter().any(|endpoint| endpoint.scheme() == "udp") {
            if let Err(reason) =
                create_udp_sink(Manager::generate_uuid(), video_and_stream_information)
                    .and_then(|sink| stream.pipeline.add_sink(sink))
            {
                return Err(anyhow!(
                    "Failed to add Sink of type UDP to the Pipeline. Reason: {reason}"
                ));
            }
        }

        if endpoints.iter().any(|endpoint| endpoint.scheme() == "rtsp") {
            if let Err(reason) =
                create_rtsp_sink(Manager::generate_uuid(), video_and_stream_information)
                    .and_then(|sink| stream.pipeline.add_sink(sink))
            {
                return Err(anyhow!(
                    "Failed to add Sink of type RTSP to the Pipeline. Reason: {reason}"
                ));
            }
        }

        if let Err(reason) =
            create_image_sink(Manager::generate_uuid(), video_and_stream_information)
                .and_then(|sink| stream.pipeline.add_sink(sink))
        {
            return Err(anyhow!(
                "Failed to add Sink of type Image to the Pipeline. Reason: {reason}"
            ));
        }

        // Start the pipeline. This will automatically start sinks with linked proxy-isolated pipelines
        stream
            .pipeline
            .inner_state_as_ref()
            .pipeline_runner
            .start()?;

        // Start all the sinks
        for sink in stream.pipeline.inner_state_mut().sinks.values() {
            sink.start()?
        }

        Ok(stream)
    }
}

impl Drop for StreamState {
    fn drop(&mut self) {
        let pipeline_state = self.pipeline.inner_state_as_ref();
        let pipeline = &pipeline_state.pipeline;

        if let Err(error) = pipeline.post_message(::gst::message::Eos::new()) {
            error!("Failed posting Eos message into Pipeline bus. Reason: {error:?}");
        }

        if let Err(error) = pipeline.set_state(::gst::State::Null) {
            error!("Failed setting Pipeline state to Null. Reason: {error:?}");
        }
        if let Err(error) = wait_for_element_state(
            pipeline.upcast_ref::<::gst::Element>(),
            ::gst::State::Null,
            100,
            10,
        ) {
            let _ = pipeline.set_state(::gst::State::Null);
            error!("Failed setting Pipeline state to Null. Reason: {error:?}");
        }

        // Remove all Sinks
        let pipeline_state = self.pipeline.inner_state_mut();
        let sink_ids = &pipeline_state
            .sinks
            .keys()
            .cloned()
            .collect::<Vec<uuid::Uuid>>();
        for sink_id in sink_ids {
            if let Err(error) = pipeline_state.remove_sink(sink_id) {
                warn!("Failed unlinking Sink {sink_id:?} from Pipeline. Reason: {error:?}");
            }
        }
    }
}

#[instrument(level = "debug")]
fn validate_endpoints(video_and_stream_information: &VideoAndStreamInformation) -> Result<()> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints.is_empty() {
        return Err(anyhow!("Endpoints are empty"));
    }

    if endpoints.iter().filter(|&e| e.scheme() == "rtsp").count() > 1 {
        return Err(anyhow!("Only one RTSP endpoint is supported at time"));
    }

    let encode = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::Video(configuration) => configuration.encode.clone(),
        CaptureConfiguration::Redirect(_) => VideoEncodeType::Unknown("".into()),
    };

    let errors: Vec<anyhow::Error> = endpoints.iter().filter_map(|endpoint| {

        let scheme = endpoint.scheme();

        if matches!(
            video_and_stream_information.video_source,
            VideoSourceType::Redirect(_)
        ) {
            match scheme {
                "udp" | "rtsp" => return None,
                _ => return Some(anyhow!(
                    "The URL's scheme for REDIRECT endpoints should be \"udp\" or \"rtsp\", but was: {scheme:?}",
                ))
            };
        }

        match scheme {
            "udp" => {
                if VideoEncodeType::H265 == encode {
                    return Some(anyhow!("Endpoint with udp scheme only supports H264, encode type is H265, the scheme should be udp265."));
                }

                // UDP endpoints should contain both host and port
                if endpoint.host().is_none() || endpoint.port().is_none()
                {
                    return Some(anyhow!(
                        "Endpoint with udp scheme should contain host and port. Endpoint: {endpoint:?}"
                    ));
                }
            }
            "udp265" => {
                if VideoEncodeType::H265 != encode {
                    return Some(anyhow!("Endpoint with udp265 scheme only supports H265 encode. Encode: {encode:?}, Endpoint: {endpoints:?}"));
                }
            }
            "rtsp" => {
                // RTSP endpoints should contain host, port, and path
                if endpoint.host().is_none() || endpoint.port().is_none() || endpoint.path().is_empty() {
                    return Some(anyhow!(
                        "Endpoint with rtsp scheme should contain host, port, and path. Endpoint: {endpoint:?}"
                    ));
                }
                if endpoint.port() != Some(RTSP_SERVER_PORT) {
                    return Some(anyhow!(
                        "Endpoint with rtsp scheme should use port {RTSP_SERVER_PORT:?}. Endpoint: {endpoint:?}"
                    ));
                }
            }
            _ => {
                return Some(anyhow!(
                    "Scheme is not accepted as stream endpoint: {scheme}"
                ));
            }
        }

        None
    }).collect();

    if !errors.is_empty() {
        return Err(anyhow!(
            "One or more endpoints are invalid. List of Errors:\n{errors:?}",
        ));
    }

    Ok(())
}
