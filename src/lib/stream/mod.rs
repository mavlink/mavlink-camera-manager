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
use sink::{create_image_sink, create_rtsp_sink, create_udp_sink, create_zenoh_sink};
use tokio::sync::RwLock;
use tracing::*;
use types::*;
use webrtc::signalling_protocol::PeerId;

use crate::{
    mavlink::mavlink_camera::MavlinkCamera,
    video::{
        types::{FrameInterval, VideoEncodeType, VideoSourceType},
        video_source::{cameras_available, VideoSource},
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
    pub state: Arc<RwLock<Option<StreamState>>>,
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

fn normalize_pipeline_source_string(source_string: &str) -> String {
    // To be DHCP-friendly, we ignore the address for IP-based sources.
    match source_string.parse::<url::Url>() {
        Ok(mut url) => {
            let _ = url.set_host(None);
            let _ = url.set_port(None);
            url.to_string()
        }
        Err(_) => source_string.to_string(),
    }
}

fn pipeline_id_seed(video_and_stream_information: &VideoAndStreamInformation) -> String {
    let video_source_inner = video_and_stream_information.video_source.inner();
    let source_string = normalize_pipeline_source_string(video_source_inner.source_string());

    if video_source_inner.is_shareable() {
        format!(
            "{}:{}:{}",
            video_and_stream_information.name,
            video_source_inner.name(),
            source_string,
        )
    } else {
        format!("{}:{}", video_source_inner.name(), source_string)
    }
}

fn generate_pipeline_id(video_and_stream_information: &VideoAndStreamInformation) -> uuid::Uuid {
    let pipeline_id_seed = pipeline_id_seed(video_and_stream_information);
    Manager::generate_uuid(Some(&pipeline_id_seed))
}

fn default_video_capture_configuration(encode: VideoEncodeType) -> VideoCaptureConfiguration {
    VideoCaptureConfiguration {
        encode,
        height: 0,
        width: 0,
        frame_interval: FrameInterval {
            numerator: 0,
            denominator: 0,
        },
    }
}

/// Re-probes the stream source to obtain a fresh `CaptureConfiguration`.
///
/// Returns `Ok(Some(config))` when the source was probed and a new configuration
/// is available, `Ok(None)` when no probing is needed (Gst, Local), or `Err`
/// when probing failed.
#[instrument(level = "debug", skip_all)]
pub(crate) async fn refresh_source_configuration(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Option<CaptureConfiguration>> {
    match &video_and_stream_information.video_source {
        VideoSourceType::Redirect(_) => {
            let url = video_and_stream_information
                .stream_information
                .endpoints
                .first()
                .context("No URL found")?;

            get_capture_configuration_from_stream_uri(url)
                .await
                .map(Some)
        }

        VideoSourceType::Onvif(source) => {
            let url = url::Url::parse(source.source_string())
                .context("Failed to parse ONVIF source URL")?;

            get_capture_configuration_from_stream_uri(&url)
                .await
                .map(Some)
        }

        VideoSourceType::Local(_) | VideoSourceType::Gst(_) => Ok(None),
    }
}

impl Stream {
    #[instrument(level = "debug", skip_all)]
    pub async fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline_id = Arc::new(generate_pipeline_id(video_and_stream_information));

        // Replace Redirect with Video
        let video_and_stream_information = {
            let mut video_and_stream_information = video_and_stream_information.clone();

            if matches!(
                video_and_stream_information.video_source,
                VideoSourceType::Redirect(_)
            ) {
                video_and_stream_information
                    .stream_information
                    .configuration =
                    CaptureConfiguration::Video(default_video_capture_configuration(
                        VideoEncodeType::Unknown("Redirect stream".to_string()),
                    ));
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

    #[instrument(level = "debug", skip(state, terminated))]
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

                // Re-probe configuration for sources that support it (Redirect, Onvif)
                match refresh_source_configuration(&video_and_stream_information_cloned).await {
                    Ok(Some(capture_configuration)) => {
                        *error_status.write().await = Ok(());

                        video_and_stream_information
                            .write()
                            .await
                            .stream_information
                            .configuration = capture_configuration;
                    }
                    Ok(None) => {
                        // No probing needed (Gst) or handled separately (Local)
                    }
                    Err(error) => {
                        let error_message = format!(
                            "Failed getting CaptureConfiguration from source. Error: {error:?}. Trying again soon..."
                        );

                        warn!(error_message);
                        *error_status.write().await = Err(anyhow!(error_message));

                        continue;
                    }
                }

                // If it's a local camera, try to update the device
                if matches!(
                    video_and_stream_information_cloned.video_source,
                    VideoSourceType::Local(_)
                ) {
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
                    *video_and_stream_information.write().await = streams.first().unwrap().clone();

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
    #[instrument(level = "debug", skip_all)]
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

    #[instrument(level = "debug", skip_all)]
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
                let sink_id = Arc::new(Manager::generate_uuid(None));
                match create_udp_sink(sink_id.clone(), &video_and_stream_information) {
                    Ok(sink) => {
                        if let Some(pipeline) = stream.pipeline.as_mut() {
                            if let Err(reason) = pipeline.add_sink(sink).await {
                                return Err(anyhow!(
                                    "Failed to add Sink of type UDP to the Pipeline. Reason: {reason}"
                                ));
                            }
                        } else {
                            return Err(anyhow!("No Pipeline available to add UDP sink"));
                        }
                    }
                    Err(reason) => {
                        return Err(anyhow!(
                            "Failed to create Sink of type UDP. Reason: {reason}"
                        ));
                    }
                }
            }

            if endpoints
                .iter()
                .any(|endpoint| RTSPScheme::try_from(endpoint.scheme()).is_ok())
            {
                let sink_id = Arc::new(Manager::generate_uuid(None));
                match create_rtsp_sink(sink_id.clone(), &video_and_stream_information) {
                    Ok(sink) => {
                        if let Some(pipeline) = stream.pipeline.as_mut() {
                            if let Err(reason) = pipeline.add_sink(sink).await {
                                return Err(anyhow!(
                                    "Failed to add Sink of type RTSP to the Pipeline. Reason: {reason}"
                                ));
                            }
                        } else {
                            return Err(anyhow!("No Pipeline available to add RTSP sink"));
                        }
                    }
                    Err(reason) => {
                        return Err(anyhow!(
                            "Failed to create Sink of type RTSP. Reason: {reason}"
                        ));
                    }
                }
            }
        }

        let sink_id = Arc::new(Manager::generate_uuid(None));
        if !video_and_stream_information
            .stream_information
            .extended_configuration
            .as_ref()
            .map(|e| e.disable_thumbnails)
            .unwrap_or_default()
        {
            match create_image_sink(sink_id.clone(), &video_and_stream_information) {
                Ok(sink) => {
                    if let Some(pipeline) = stream.pipeline.as_mut() {
                        if let Err(reason) = pipeline.add_sink(sink).await {
                            return Err(anyhow!(
                            "Failed to add Sink of type Image to the Pipeline. Reason: {reason}"
                        ));
                        }
                    } else {
                        return Err(anyhow!("No Pipeline available to add Image sink"));
                    }
                }
                Err(reason) => {
                    return Err(anyhow!(
                        "Failed to create Sink of type Image. Reason: {reason}"
                    ));
                }
            }
        }

        if !video_and_stream_information
            .stream_information
            .extended_configuration
            .as_ref()
            .map(|e| e.disable_zenoh)
            .unwrap_or_default()
            && crate::cli::manager::enable_zenoh()
        {
            let encoding = match &video_and_stream_information
                .stream_information
                .configuration
            {
                CaptureConfiguration::Video(video_configuraiton) => {
                    video_configuraiton.encode.clone()
                }
                CaptureConfiguration::Redirect(_) => {
                    return Err(anyhow!(
                        "Redirect CaptureConfiguration means the stream was not initialized yet"
                    ));
                }
            };

            if matches!(encoding, VideoEncodeType::H264 | VideoEncodeType::H265) {
                let sink_id = Arc::new(Manager::generate_uuid(None));
                match create_zenoh_sink(sink_id.clone(), &video_and_stream_information).await {
                    Ok(sink) => {
                        if let Some(pipeline) = stream.pipeline.as_mut() {
                            if let Err(reason) = pipeline.add_sink(sink).await {
                                return Err(anyhow!(
                                "Failed to add Sink of type Zenoh to the Pipeline. Reason: {reason}"
                            ));
                            }
                        } else {
                            return Err(anyhow!("No Pipeline available to add Zenoh sink"));
                        }
                    }
                    Err(reason) => {
                        return Err(anyhow!(
                            "Failed to create Sink of type Zenoh. Reason: {reason}"
                        ));
                    }
                }
            } else {
                debug!(
                    "Zenoh Sink was not added because the encoding {encoding:?} is not supported"
                );
            }
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
        if let Some(pipeline) = stream.pipeline.as_mut() {
            let pipeline_state = pipeline.inner_state_mut();
            for sink in pipeline_state.sinks.values() {
                if let Err(error) = sink.start() {
                    warn!("Failed to start sink: {error:?}");
                }
            }
        }

        // Only create the MavlinkCamera when MAVLink is not disabled
        if video_and_stream_information
            .stream_information
            .extended_configuration
            .as_ref()
            .map(|e| !e.disable_mavlink)
            .unwrap_or_default()
        {
            match MavlinkCamera::try_new(&video_and_stream_information).await {
                Ok(mavlink_camera) => {
                    stream.mavlink_camera.replace(mavlink_camera);
                }
                Err(error) => {
                    warn!("Failed to create MavlinkCamera: {error:?}");
                }
            }
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

#[instrument(level = "debug", skip_all)]
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

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use url::Url;

    use super::*;
    #[cfg(target_os = "linux")]
    use crate::video::video_source_local::{VideoSourceLocal, VideoSourceLocalType};
    use crate::video::video_source_redirect::{VideoSourceRedirect, VideoSourceRedirectType};

    fn test_stream_information() -> StreamInformation {
        StreamInformation {
            endpoints: vec![Url::parse("rtsp://127.0.0.1:8554/test").unwrap()],
            configuration: CaptureConfiguration::Video(default_video_capture_configuration(
                VideoEncodeType::H264,
            )),
            extended_configuration: None,
        }
    }

    fn redirect_stream(name: &str, endpoint: &str) -> VideoAndStreamInformation {
        VideoAndStreamInformation {
            name: name.to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(endpoint).unwrap()],
                ..test_stream_information()
            },
            video_source: VideoSourceType::Redirect(VideoSourceRedirect {
                name: "Redirect source".into(),
                source: VideoSourceRedirectType::Redirect("Redirect".into()),
            }),
        }
    }

    fn settings_file() -> String {
        format!("/tmp/stream-tests-{}.json", uuid::Uuid::new_v4())
    }

    #[test]
    fn shareable_redirect_streams_use_their_name_in_pipeline_id() {
        let first = generate_pipeline_id(&redirect_stream(
            "yard-east",
            "rtsp://127.0.0.1:8554/yard-east",
        ));
        let second = generate_pipeline_id(&redirect_stream(
            "yard-west",
            "rtsp://127.0.0.1:8554/yard-west",
        ));

        assert_ne!(first, second);
    }

    #[test]
    fn redirect_pipeline_id_is_stable_for_the_same_stream_input() {
        let stream = redirect_stream("yard-east", "rtsp://127.0.0.1:8554/yard-east");

        assert_eq!(generate_pipeline_id(&stream), generate_pipeline_id(&stream));
    }

    #[tokio::test]
    #[serial]
    async fn redirect_streams_can_be_added_without_pipeline_id_collisions() {
        crate::settings::manager::init(Some(&settings_file())).await;
        crate::settings::manager::clear_blocked_sources();
        crate::stream::manager::remove_all_streams().await.unwrap();

        let first = redirect_stream("yard-east", "rtsp://127.0.0.1:8554/yard-east");
        let second = redirect_stream("yard-west", "rtsp://127.0.0.1:8554/yard-west");

        crate::stream::manager::add_stream_and_start(first)
            .await
            .unwrap();
        crate::stream::manager::add_stream_and_start(second)
            .await
            .unwrap();

        let streams = crate::stream::manager::streams().await.unwrap();
        let stream_ids: std::collections::HashSet<_> =
            streams.iter().map(|stream| stream.id).collect();

        assert_eq!(streams.len(), 2);
        assert_eq!(stream_ids.len(), 2);

        crate::stream::manager::remove_stream_by_name("yard-east")
            .await
            .unwrap();
        crate::stream::manager::remove_stream_by_name("yard-west")
            .await
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn non_shareable_sources_keep_host_and_port_out_of_the_seed() {
        let first = VideoAndStreamInformation {
            name: "cam-a".into(),
            stream_information: test_stream_information(),
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: "Local camera".into(),
                device_path: "rtsp://10.23.8.41:8554/yard-east".into(),
                typ: VideoSourceLocalType::Unknown("test".into()),
            }),
        };
        let second = VideoAndStreamInformation {
            name: "cam-b".into(),
            stream_information: test_stream_information(),
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: "Local camera".into(),
                device_path: "rtsp://192.168.2.10:9000/yard-east".into(),
                typ: VideoSourceLocalType::Unknown("test".into()),
            }),
        };

        assert_eq!(generate_pipeline_id(&first), generate_pipeline_id(&second));
    }

    use crate::controls::onvif::camera::OnvifDeviceInformation;
    use crate::video::video_source_onvif::{VideoSourceOnvif, VideoSourceOnvifType};

    fn allocate_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().port()
    }

    fn spawn_h264_udp_sender(port: u16) -> ::gst::Pipeline {
        ::gst::init().unwrap();
        let pipeline = ::gst::parse::launch(&format!(
            concat!(
                "videotestsrc is-live=true pattern=ball do-timestamp=true",
                " ! video/x-raw,width=160,height=120,framerate=30/1",
                " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=500",
                " ! h264parse config-interval=-1",
                " ! rtph264pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                " ! udpsink host=127.0.0.1 port={port}",
            ),
            port = port,
        ))
        .unwrap()
        .downcast::<::gst::Pipeline>()
        .unwrap();
        pipeline.set_state(::gst::State::Playing).unwrap();
        pipeline
    }

    fn spawn_h265_udp_sender(port: u16) -> ::gst::Pipeline {
        ::gst::init().unwrap();
        let pipeline = ::gst::parse::launch(&format!(
            concat!(
                "videotestsrc is-live=true pattern=ball do-timestamp=true",
                " ! video/x-raw,width=160,height=120,framerate=30/1,format=I420",
                " ! x265enc tune=zerolatency speed-preset=ultrafast bitrate=500",
                " ! h265parse config-interval=-1",
                " ! rtph265pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                " ! udpsink host=127.0.0.1 port={port}",
            ),
            port = port,
        ))
        .unwrap()
        .downcast::<::gst::Pipeline>()
        .unwrap();
        pipeline.set_state(::gst::State::Playing).unwrap();
        pipeline
    }

    fn onvif_stream(name: &str, source_url: &str) -> VideoAndStreamInformation {
        VideoAndStreamInformation {
            name: name.to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("rtsp://127.0.0.1:8554/test").unwrap()],
                configuration: CaptureConfiguration::Video(default_video_capture_configuration(
                    VideoEncodeType::H264,
                )),
                extended_configuration: None,
            },
            video_source: VideoSourceType::Onvif(VideoSourceOnvif {
                name: "Test ONVIF Camera".into(),
                source: VideoSourceOnvifType::Onvif(source_url.to_string()),
                device_information: OnvifDeviceInformation {
                    manufacturer: "Test".into(),
                    model: "Test".into(),
                    firmware_version: "1.0".into(),
                    serial_number: "000".into(),
                    hardware_id: "test".into(),
                },
            }),
        }
    }

    #[tokio::test]
    async fn refresh_source_configuration_redirect_detects_h264() {
        let port = allocate_udp_port();
        let sender = spawn_h264_udp_sender(port);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let stream = redirect_stream("test-redirect", &format!("udp://127.0.0.1:{port}"));
        let result = refresh_source_configuration(&stream).await;
        sender.set_state(::gst::State::Null).unwrap();

        let config = result
            .expect("probe should succeed")
            .expect("should return Some");
        let CaptureConfiguration::Video(video_config) = config else {
            panic!("expected Video configuration");
        };
        assert_eq!(video_config.encode, VideoEncodeType::H264);
    }

    #[tokio::test]
    async fn refresh_source_configuration_redirect_detects_encoding_change() {
        let port = allocate_udp_port();

        let sender = spawn_h264_udp_sender(port);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let stream = redirect_stream("test-redirect", &format!("udp://127.0.0.1:{port}"));
        let result = refresh_source_configuration(&stream).await;
        sender.set_state(::gst::State::Null).unwrap();

        let config = result.unwrap().unwrap();
        let CaptureConfiguration::Video(video_config) = config else {
            panic!("expected Video configuration");
        };
        assert_eq!(video_config.encode, VideoEncodeType::H264);

        let sender = spawn_h265_udp_sender(port);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let result = refresh_source_configuration(&stream).await;
        sender.set_state(::gst::State::Null).unwrap();

        let config = result.unwrap().unwrap();
        let CaptureConfiguration::Video(video_config) = config else {
            panic!("expected Video configuration");
        };
        assert_eq!(video_config.encode, VideoEncodeType::H265);
    }

    #[tokio::test]
    async fn refresh_source_configuration_onvif_detects_h264() {
        let port = allocate_udp_port();
        let sender = spawn_h264_udp_sender(port);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let stream = onvif_stream("test-onvif", &format!("udp://127.0.0.1:{port}"));
        let result = refresh_source_configuration(&stream).await;
        sender.set_state(::gst::State::Null).unwrap();

        let config = result
            .expect("probe should succeed")
            .expect("should return Some");
        let CaptureConfiguration::Video(video_config) = config else {
            panic!("expected Video configuration");
        };
        assert_eq!(video_config.encode, VideoEncodeType::H264);
    }

    #[tokio::test]
    async fn refresh_source_configuration_onvif_detects_encoding_change() {
        let port = allocate_udp_port();

        let sender = spawn_h264_udp_sender(port);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let stream = onvif_stream("test-onvif", &format!("udp://127.0.0.1:{port}"));
        let result = refresh_source_configuration(&stream).await;
        sender.set_state(::gst::State::Null).unwrap();

        let config = result.unwrap().unwrap();
        let CaptureConfiguration::Video(video_config) = config else {
            panic!("expected Video configuration");
        };
        assert_eq!(video_config.encode, VideoEncodeType::H264);

        let sender = spawn_h265_udp_sender(port);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let result = refresh_source_configuration(&stream).await;
        sender.set_state(::gst::State::Null).unwrap();

        let config = result.unwrap().unwrap();
        let CaptureConfiguration::Video(video_config) = config else {
            panic!("expected Video configuration");
        };
        assert_eq!(video_config.encode, VideoEncodeType::H265);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn refresh_source_configuration_returns_none_for_local() {
        let stream = VideoAndStreamInformation {
            name: "test-local".into(),
            stream_information: test_stream_information(),
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: "Local camera".into(),
                device_path: "/dev/video0".into(),
                typ: VideoSourceLocalType::Unknown("test".into()),
            }),
        };

        let result = refresh_source_configuration(&stream).await;
        assert!(result.unwrap().is_none());
    }
}
