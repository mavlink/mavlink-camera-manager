use std::sync::Arc;

use anyhow::{anyhow, Result};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::types::CaptureConfiguration,
    video::{
        types::{VideoEncodeType, VideoSourceType},
        video_source_gst::VideoSourceGstType,
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PipelineGstreamerInterface, PipelineState, PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME,
    PIPELINE_VIDEO_TEE_NAME,
};

#[derive(Debug)]
pub struct QrPipeline {
    pub state: PipelineState,
}

impl QrPipeline {
    #[instrument(level = "debug")]
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
                    "{unsupported:?} is not supported as QrTimeStamp Pipeline"
                ))
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "VideoSourceType {unsupported:?} is not supported as QrTimeStamp Pipeline"
                ))
            }
        };

        let _pattern = match &video_source.source {
            VideoSourceGstType::QR(pattern) => pattern,
            unsupported => {
                return Err(anyhow!(
                    "VideoSourceGstType {unsupported:?} is not supported as QrTimeStamp Pipeline"
                ))
            }
        };

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                format!(concat!(
                        "qrtimestampsrc",
                        " ! video/x-raw,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! videoconvert",
                        " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                        " ! h264parse",
                        " ! capsfilter name={filter_name} caps=video/x-h264,profile={profile},stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    profile = "constrained-baseline",
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Rgb => {
                format!(
                    concat!(
                        "qrtimestampsrc",
                        " ! video/x-raw,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpvrawpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for Test Pipeline"
                ))
            }
        };

        debug!("pipeline_description: {description:#?}");
        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for QrPipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
