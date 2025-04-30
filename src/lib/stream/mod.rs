pub mod gst;
pub mod manager;
pub mod pipeline;
pub mod rtsp;
pub mod sink;
pub mod types;
pub mod webrtc;

use std::sync::Arc;

use ::gst::prelude::*;
use anyhow::{anyhow, Context, Result};
use gst::utils::get_capture_configuration_from_stream_uri;
use manager::Manager;
use pipeline::{Pipeline, PipelineGstreamerInterface};
use sink::{create_file_sink, create_image_sink, create_rtsp_sink, create_udp_sink};
use tokio::sync::RwLock;
use tracing::*;
use types::*;
use webrtc::signalling_protocol::PeerId;

use crate::{
    mavlink::mavlink_camera::MavlinkCamera,
    video::{
        types::{FrameInterval, VideoEncodeType, VideoSourceType},
        video_source::cameras_available,
    },
    video_stream::types::VideoAndStreamInformation,
};

use self::{
    gst::utils::wait_for_element_state,
    rtsp::{rtsp_scheme::RTSPScheme, rtsp_server::RTSP_SERVER_PORT},
    sink::SinkInterface,
};

#[derive(Debug)]
pub struct Stream {
    state: Arc<RwLock<Option<StreamState>>>,
    pipeline_id: Arc<PeerId>,
    video_and_stream_information: Arc<RwLock<VideoAndStreamInformation>>,
    error: Arc<RwLock<anyhow::Result<()>>>,
    terminated: Arc<RwLock<bool>>,
    watcher_handle: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug)]
pub struct StreamState {
    pub pipeline_id: Arc<PeerId>,
    pub pipeline: Option<Pipeline>,
    pub video_and_stream_information: Arc<RwLock<VideoAndStreamInformation>>,
    pub mavlink_camera: Option<MavlinkCamera>,
}

impl Stream {
    #[instrument(level = "debug")]
    pub async fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline_id = Arc::new(Manager::generate_uuid());

        // Replace Redirect with Video
        let video_and_stream_information = {
            let mut video_and_stream_information = video_and_stream_information.clone();

            if matches!(
                video_and_stream_information.video_source,
                VideoSourceType::Redirect(_)
            ) {
                video_and_stream_information
                    .stream_information
                    .configuration = CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: VideoEncodeType::Unknown("Redirect stream".to_string()),
                    height: 0,
                    width: 0,
                    frame_interval: FrameInterval {
                        numerator: 0,
                        denominator: 0,
                    },
                });
            }

            Arc::new(RwLock::new(video_and_stream_information))
        };

        let state = Arc::new(RwLock::new(Some(
            StreamState::try_default(video_and_stream_information.clone(), pipeline_id.clone())
                .await?,
        )));

        let terminated = Arc::new(RwLock::new(false));
        let error = Arc::new(RwLock::new(Ok(())));

        debug!("Starting StreamWatcher task...");

        let watcher_handle = Some(tokio::spawn({
            let terminated = terminated.clone();
            let error = error.clone();
            let video_and_stream_information = video_and_stream_information.clone();
            let state = state.clone();
            let pipeline_id = pipeline_id.clone();

            async move {
                debug!("StreamWatcher task started!");
                match Self::watcher(
                    video_and_stream_information,
                    pipeline_id,
                    error,
                    state,
                    terminated,
                )
                .await
                {
                    Ok(_) => debug!("StreamWatcher task eneded with no errors"),
                    Err(error) => warn!("StreamWatcher task ended with error: {error:#?}"),
                };
            }
        }));

        Ok(Self {
            pipeline_id,
            video_and_stream_information,
            error,
            state,
            terminated,
            watcher_handle,
        })
    }

    #[instrument(level = "debug", skip(video_and_stream_information, state, terminated))]
    async fn watcher(
        video_and_stream_information: Arc<RwLock<VideoAndStreamInformation>>,
        pipeline_id: Arc<uuid::Uuid>,
        error_status: Arc<RwLock<anyhow::Result<()>>>,
        state: Arc<RwLock<Option<StreamState>>>,
        terminated: Arc<RwLock<bool>>,
    ) -> Result<()> {
        // To reduce log size, each report we raise the report interval geometrically until a maximum value is reached:
        let report_interval_mult = 2;
        let report_interval_max = 60;
        let mut report_interval = std::time::Duration::from_secs(1);
        let mut last_report_time = std::time::Instant::now();

        let mut period = tokio::time::interval(tokio::time::Duration::from_millis(100));
        loop {
            period.tick().await;

            if !state.read().await.as_ref().is_some_and(|state| {
                state
                    .pipeline
                    .as_ref()
                    .map(|pipeline| pipeline.is_running())
                    .unwrap_or_default()
            }) {
                // First, drop the current state
                if let Some(state) = state.write().await.take() {
                    drop(state);
                }

                let video_and_stream_information_cloned =
                    video_and_stream_information.read().await.clone();

                match video_and_stream_information_cloned.video_source {
                    // If it's a redirect, update CaptureConfiguration as a CaptureConfiguration::Video
                    VideoSourceType::Redirect(_) => {
                        let url = video_and_stream_information_cloned
                            .stream_information
                            .endpoints
                            .first()
                            .context("No URL found")?;

                        let capture_configuration = match get_capture_configuration_from_stream_uri(
                            url,
                        )
                        .await
                        {
                            Ok(capture_configuration) => capture_configuration,
                            Err(error) => {
                                let error_message =
                                        format!("Failed getting CaptureConfiguration from endpoint. Error: {error:?}. Trying again soon...");

                                warn!(error_message);
                                *error_status.write().await = Err(anyhow!(error_message));

                                continue;
                            }
                        };

                        *error_status.write().await = Ok(());

                        video_and_stream_information
                            .write()
                            .await
                            .stream_information
                            .configuration = capture_configuration
                    }

                    // If it's a camera, try to update the device
                    VideoSourceType::Local(_) => {
                        let mut streams = vec![video_and_stream_information_cloned.clone()];
                        let mut candidates = cameras_available().await;

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
                        manager::update_devices(&mut streams, &mut candidates, should_report).await;
                        *video_and_stream_information.write().await =
                            streams.first().unwrap().clone();

                        // Check if the chosen video source is available
                        match crate::video::video_source::get_video_source(
                            video_and_stream_information_cloned
                                .video_source
                                .inner()
                                .source_string(),
                        )
                        .await
                        {
                            Ok(best_candidate) => {
                                video_and_stream_information.write().await.video_source =
                                    best_candidate;
                            }
                            Err(error) => {
                                if should_report {
                                    let error_message  = format!("Failed to recreate the stream {pipeline_id:?}: {error:?}. Is the device connected? Trying again each second until the success or stream is removed. Next report in {report_interval:?} to reduce log size.");

                                    warn!(error_message);
                                    *error_status.write().await = Err(anyhow!(error_message));

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

                    VideoSourceType::Gst(_) => (),

                    VideoSourceType::Onvif(_) => (),
                }

                let new_state = match StreamState::try_new(
                    video_and_stream_information.clone(),
                    pipeline_id.clone(),
                )
                .await
                {
                    Ok(state) => state,
                    Err(error) => {
                        let error_message=  format!("Failed to recreate the stream {pipeline_id:?}: {error:#?}. Trying again in one second...");

                        warn!(error_message);
                        *error_status.write().await = Err(anyhow!(error_message));

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
    pub async fn try_default(
        video_and_stream_information: Arc<RwLock<VideoAndStreamInformation>>,
        pipeline_id: Arc<uuid::Uuid>,
    ) -> Result<Self> {
        if let Err(error) = validate_endpoints(&video_and_stream_information.read().await.clone()) {
            return Err(anyhow!("Failed validating endpoints. Reason: {error:?}"));
        }

        Ok(StreamState {
            pipeline_id,
            pipeline: None,
            video_and_stream_information,
            mavlink_camera: None,
        })
    }

    #[instrument(level = "debug")]
    pub async fn try_new(
        video_and_stream_information: Arc<RwLock<VideoAndStreamInformation>>,
        pipeline_id: Arc<uuid::Uuid>,
    ) -> Result<Self> {
        let mut stream =
            Self::try_default(video_and_stream_information.clone(), pipeline_id.clone()).await?;

        let video_and_stream_information = video_and_stream_information.read().await.clone();

        stream.pipeline = Some(Pipeline::try_new(
            &video_and_stream_information,
            &pipeline_id,
        )?);

        // Do not add any Sink if it's a redirect Pipeline
        // FIXME: RE-EVALUATE THIS!!!
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
                if let Err(reason) = create_udp_sink(
                    Arc::new(Manager::generate_uuid()),
                    &video_and_stream_information,
                )
                .and_then(|sink| {
                    stream
                        .pipeline
                        .as_mut()
                        .context("No Pileine")?
                        .add_sink(sink)
                }) {
                    return Err(anyhow!(
                        "Failed to add Sink of type UDP to the Pipeline. Reason: {reason}"
                    ));
                }
            }

            if endpoints
                .iter()
                .any(|endpoint| RTSPScheme::try_from(endpoint.scheme()).is_ok())
            {
                if let Err(reason) = create_rtsp_sink(
                    Arc::new(Manager::generate_uuid()),
                    &video_and_stream_information,
                )
                .and_then(|sink| {
                    stream
                        .pipeline
                        .as_mut()
                        .context("No Pileine")?
                        .add_sink(sink)
                }) {
                    return Err(anyhow!(
                        "Failed to add Sink of type RTSP to the Pipeline. Reason: {reason}"
                    ));
                }
            }
        }

        if let Err(reason) = create_image_sink(
            Arc::new(Manager::generate_uuid()),
            &video_and_stream_information,
        )
        .and_then(|sink| {
            stream
                .pipeline
                .as_mut()
                .context("No Pileine")?
                .add_sink(sink)
        }) {
            return Err(anyhow!(
                "Failed to add Sink of type Image to the Pipeline. Reason: {reason}"
            ));
        }

        if let Err(reason) = create_file_sink(
            Arc::new(Manager::generate_uuid()),
            &video_and_stream_information,
        )
        .and_then(|sink| {
            stream
                .pipeline
                .as_mut()
                .context("No Pileine")?
                .add_sink(sink)
        }) {
            return Err(anyhow!(
                "Failed to add Sink of type File to the Pipeline. Reason: {reason}"
            ));
        }

        // Start the pipeline. This will automatically start sinks with linked proxy-isolated pipelines
        stream
            .pipeline
            .as_ref()
            .context("No Pipeline")?
            .inner_state_as_ref()
            .pipeline_runner
            .start()?;

        // Start all the sinks
        for sink in stream
            .pipeline
            .as_mut()
            .context("No Pipeline")?
            .inner_state_mut()
            .sinks
            .values()
        {
            sink.start()?
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
            stream.mavlink_camera = MavlinkCamera::try_new(&video_and_stream_information)
                .await
                .ok();
        }

        Ok(stream)
    }
}

impl Drop for StreamState {
    #[instrument(level = "debug", skip(self), fields(pipeline_id = self.pipeline_id.to_string()))]
    fn drop(&mut self) {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return;
        };

        let pipeline_state = pipeline.inner_state_as_ref();
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
        let pipeline_state = self
            .pipeline
            .as_mut()
            .expect("No Pipeline")
            .inner_state_mut();
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
        CaptureConfiguration::Redirect(_) => VideoEncodeType::Unknown("Redirect stream".into()),
    };

    let errors: Vec<anyhow::Error> = endpoints.iter().filter_map(|endpoint| {

        let scheme = endpoint.scheme();

        if matches!(
            video_and_stream_information.video_source,
            VideoSourceType::Redirect(_)
        ) {
            match scheme {
                "udp" | "udp265" | "rtsp" => return None,
                _ => return Some(anyhow!(
                    "The URL's scheme for REDIRECT endpoints should be \"udp\", \"udp265\", or \"rtsp\", but was: {scheme:?}",
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
