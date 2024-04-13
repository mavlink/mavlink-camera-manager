pub mod gst;
pub mod manager;
pub mod pipeline;
pub mod rtsp;
pub mod sink;
pub mod types;
pub mod webrtc;

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::mavlink::mavlink_camera::MavlinkCamera;
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video::video_source::cameras_available;
use crate::video_stream::types::VideoAndStreamInformation;

use manager::Manager;
use pipeline::Pipeline;
use sink::{create_image_sink, create_rtsp_sink, create_udp_sink};
use types::*;
use webrtc::signalling_protocol::PeerId;

use anyhow::{anyhow, Result};

use tracing::*;

use self::gst::utils::wait_for_element_state;
use self::rtsp::rtsp_scheme::RTSPScheme;
use self::rtsp::rtsp_server::RTSP_SERVER_PORT;
use self::sink::SinkInterface;

use ::gst::prelude::*;

#[derive(Debug)]
pub struct Stream {
    state: Arc<RwLock<Option<StreamState>>>,
    terminated: Arc<RwLock<bool>>,
    watcher_handle: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug)]
pub struct StreamState {
    pub pipeline_id: PeerId,
    pub pipeline: Pipeline,
    pub video_and_stream_information: VideoAndStreamInformation,
    pub mavlink_camera: Option<MavlinkCamera>,
}

impl Stream {
    #[instrument(level = "debug")]
    pub async fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline_id = Manager::generate_uuid();

        let state = Arc::new(RwLock::new(Some(
            StreamState::try_new(video_and_stream_information, &pipeline_id).await?,
        )));

        let terminated = Arc::new(RwLock::new(false));
        let terminated_cloned = terminated.clone();

        debug!("Starting StreamWatcher task...");

        let video_and_stream_information_cloned = video_and_stream_information.clone();
        let state_cloned = state.clone();
        let watcher_handle = Some(tokio::spawn(async move {
            debug!("StreamWatcher task started!");
            match Self::watcher(
                video_and_stream_information_cloned,
                pipeline_id,
                state_cloned,
                terminated_cloned,
            )
            .await
            {
                Ok(_) => debug!("StreamWatcher task eneded with no errors"),
                Err(error) => warn!("StreamWatcher task ended with error: {error:#?}"),
            };
        }));

        Ok(Self {
            state,
            terminated,
            watcher_handle,
        })
    }

    #[instrument(level = "debug", skip(video_and_stream_information, state, terminated))]
    async fn watcher(
        video_and_stream_information: VideoAndStreamInformation,
        pipeline_id: uuid::Uuid,
        state: Arc<RwLock<Option<StreamState>>>,
        terminated: Arc<RwLock<bool>>,
    ) -> Result<()> {
        // To reduce log size, each report we raise the report interval geometrically until a maximum value is reached:
        let report_interval_mult = 2;
        let report_interval_max = 60;
        let mut report_interval = std::time::Duration::from_secs(1);
        let mut last_report_time = std::time::Instant::now();

        let mut video_and_stream_information = video_and_stream_information;

        let mut period = tokio::time::interval(tokio::time::Duration::from_millis(100));
        loop {
            period.tick().await;

            if !state.read().await.as_ref().is_some_and(|state| {
                state
                    .pipeline
                    .inner_state_as_ref()
                    .pipeline_runner
                    .is_running()
            }) {
                // First, drop the current state
                if let Some(state) = state.write().await.take() {
                    drop(state);
                }

                // If it's a camera, try to update the device
                if let VideoSourceType::Local(_) = video_and_stream_information.video_source {
                    let mut streams = vec![video_and_stream_information.clone()];
                    let mut candidates = cameras_available();

                    // Discards any source from other running streams, otherwise we'd be trying to create a stream from a device in use (which is not possible)
                    let current_running_streams = manager::streams()
                        .await
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

                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                }

                let new_state = match StreamState::try_new(
                    &video_and_stream_information,
                    &pipeline_id,
                )
                .await
                {
                    Ok(state) => state,
                    Err(error) => {
                        error!("Failed to recreate the stream {pipeline_id:?}: {error:#?}. Trying again in one second...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        continue;
                    }
                };

                // Try to recreate the stream
                state.write().await.replace(new_state);
            }

            if *terminated.read().await {
                debug!("Ending stream {pipeline_id:?}.");
                break;
            }
        }

        Ok(())
    }
}

impl Drop for Stream {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        if let Some(handle) = self.watcher_handle.take() {
            let state = self.state.clone();
            let terminated = self.terminated.clone();

            std::thread::Builder::new()
                .name("Stream::Drop".to_string())
                .spawn(move || {
                    let pipeline_id = state
                        .blocking_read()
                        .as_ref()
                        .map(|state| state.pipeline_id.clone().to_string());

                    debug!(pipeline_id, "Dropping Stream...");

                    *terminated.blocking_write() = true;

                    if !handle.is_finished() {
                        handle.abort();

                        // futures::executor::block_on(async move {
                        //     let _ = handle.await;
                        //     debug!(pipeline_id, "PipelineWatcher task aborted");
                        // });
                    } else {
                        debug!(pipeline_id, "PipelineWatcher task nicely finished!");
                    }
                })
                .unwrap()
                .join()
                .unwrap()
        }
    }
}

impl StreamState {
    #[instrument(level = "debug")]
    pub async fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        pipeline_id: &uuid::Uuid,
    ) -> Result<Self> {
        if let Err(error) = validate_endpoints(video_and_stream_information) {
            return Err(anyhow!("Failed validating endpoints. Reason: {error:?}"));
        }

        let pipeline = Pipeline::try_new(video_and_stream_information, pipeline_id)?;

        let mut stream = StreamState {
            pipeline_id: *pipeline_id,
            pipeline,
            video_and_stream_information: video_and_stream_information.clone(),
            mavlink_camera: None,
        };

        // Do not add any Sink if it's a redirect Pipeline
        if !matches!(
            &video_and_stream_information.video_source,
            VideoSourceType::Redirect(_)
        ) {
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

            if endpoints
                .iter()
                .any(|endpoint| RTSPScheme::try_from(endpoint.scheme()).is_ok())
            {
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
        }

        // Only create the MavlinkCamera when MAVLink is not disabled
        if matches!(
            video_and_stream_information
                .stream_information
                .extended_configuration,
            Some(ExtendedConfiguration {
                thermal: _,
                disable_mavlink: false,
            })
        ) {
            stream.mavlink_camera = MavlinkCamera::try_new(video_and_stream_information)
                .await
                .ok();
        }

        Ok(stream)
    }
}

impl Drop for StreamState {
    #[instrument(level = "debug", skip(self), fields(pipeline_id = self.pipeline_id.to_string()))]
    fn drop(&mut self) {
        let pipeline_state = self.pipeline.inner_state_as_ref();
        let pipeline = &pipeline_state.pipeline;

        let pipeline_weak = pipeline.downgrade();
        std::thread::spawn(move || {
            let pipeline = pipeline_weak.upgrade().unwrap();
            if let Err(error) = pipeline.post_message(::gst::message::Eos::new()) {
                error!("Failed posting Eos message into Pipeline bus. Reason: {error:?}");
            }
        });

        if let Err(error) = pipeline.set_state(::gst::State::Null) {
            error!("Failed setting Pipeline state to Null. Reason: {error:?}");
        }
        if let Err(error) =
            wait_for_element_state(pipeline.downgrade(), ::gst::State::Null, 100, 10)
        {
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

        if scheme.starts_with("rtsp") {
            if RTSPScheme::try_from(scheme).is_err() {
                return Some(anyhow!(
                    "Endpoint with rtsp scheme should use one of the following variant schemes: {:?}. Endpoint: {endpoint:?}",
                    RTSPScheme::VALUES
                ));
            }

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

            return None;
        };

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
