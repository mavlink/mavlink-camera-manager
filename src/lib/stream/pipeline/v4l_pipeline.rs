use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::types::CaptureConfiguration,
    video::{
        gst_device_monitor,
        types::{VideoEncodeType, VideoSourceType},
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME,
    PipelineGstreamerInterface, PipelineState,
};

#[derive(Debug)]
pub struct V4lPipeline {
    pub state: PipelineState,
}

impl V4lPipeline {
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        pipeline_id: &Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(configuration) => configuration,
            unsupported => {
                return Err(anyhow!(
                    "{unsupported:?} is not supported as Local Pipeline"
                ));
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Local(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as Local Pipeline"
                ));
            }
        };

        let device_path = video_source.device_path.as_str();
        let width = configuration.width;
        let height = configuration.height;
        let interval_numerator = configuration.frame_interval.numerator;
        let interval_denominator = configuration.frame_interval.denominator;
        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        debug!("Building Local pipeline for device path: {device_path}");

        let device = gst_device_monitor::local_device_with_path(device_path)?
            .upgrade()
            .context("Device disappeared between selection and pipeline build")?;

        let factory_name = device
            .create_element(None)
            .context("Failed to probe device factory")?
            .factory()
            .map(|f| f.name().to_string())
            .context("Source element has no factory")?;

        debug!("Local pipeline source factory: {factory_name}");

        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        " ! h264parse config-interval=-1", // Here we need the parse to help the stream-format and alignment part, which is being fixated here because avc/au seems to reduce the CPU usage in the RTP payloading part.
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::H265 => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        " ! h265parse",
                        " ! capsfilter name={filter_name} caps=video/x-h265,stream-format=byte-stream,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph265pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Yuyv | VideoEncodeType::Nv12 => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        " ! videoconvert",
                        " ! capsfilter name={filter_name} caps=video/x-raw,format=I420,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpvrawpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Mjpg => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        // We don't need a jpegparse, as it leads to incompatible caps, spoiling the negotiation.
                        " ! capsfilter name={filter_name} caps=image/jpeg,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpjpegpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for Local Pipeline"
                ));
            }
        };

        debug!("pipeline_description: {description:#?}");

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        let source = pipeline
            .by_name("source")
            .context("Failed to find source element after parse::launch")?;

        // `do-timestamp` only exists on GstBaseSrc subclasses (v4l2src does, libcamerasrc doesn't).
        if source.has_property("do-timestamp") {
            source.set_property("do-timestamp", true);
        }

        // The v4l2 device provider's `reconfigure_element` vfunc is broken
        // upstream (it compares the factory name against the GType name), so
        // set the device-identifying property directly from the known path.
        // Other factories fall back to `reconfigure_element`, which is our
        // best-effort for now.
        // In the next iteration, we should refactor the pipeline construction
        // so we create the pipeline's element programatically, and then we
        // can use the given Device factory directly.
        match factory_name.as_str() {
            "v4l2src" => {
                source.set_property("device", device_path);
                debug!("Applied v4l2src device={device_path:?}");
            }
            "libcamerasrc" => {
                let camera_name = device.display_name();
                source.set_property("camera-name", camera_name.as_str());
                debug!("Applied libcamerasrc camera-name={camera_name:?}");
            }
            other => {
                device.reconfigure_element(&source).with_context(|| {
                    format!("Failed to apply device configuration to {other} source")
                })?;
                debug!("Applied device configuration via reconfigure_element for {other}");
            }
        }

        pipeline.set_property("name", format!("pipeline-local-{pipeline_id}"));

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for V4lPipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
