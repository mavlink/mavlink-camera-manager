use std::sync::Arc;

use anyhow::{anyhow, Result};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::types::CaptureConfiguration,
    video::{
        types::{VideoEncodeType, VideoSourceType},
        video_source_onvif::VideoSourceOnvifType,
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PipelineGstreamerInterface, PipelineState, PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME,
    PIPELINE_VIDEO_TEE_NAME,
};

#[derive(Debug)]
pub struct OnvifPipeline {
    pub state: PipelineState,
}

impl OnvifPipeline {
    #[instrument(level = "debug")]
    pub fn try_new(
        pipeline_id: &Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(configuration) => configuration,
            unsupported => {
                return Err(anyhow!(
                    "{unsupported:?} is not supported as Onvif Pipeline"
                ))
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Onvif(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as V4l Pipeline"
                ))
            }
        };

        let location = {
            let VideoSourceOnvifType::Onvif(url) = &video_source.source;
            url.to_string()
        };

        let encode = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(configuration) => Some(configuration.encode.clone()),
            _unknown => None,
        };

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let description = match encode {
            Some(VideoEncodeType::H264) => {
                format!(
                    concat!(
                        "rtspsrc location={location} is-live=true latency=0",
                        " ! application/x-rtp",
                        " ! rtph264depay",
                        // " ! h264parse", // we might want to add this in the future to expand the compatibility, since it can transform the stream format
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    location = location,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            Some(VideoEncodeType::H265) => {
                format!(
                    concat!(
                        "rtspsrc location={location} is-live=true latency=0",
                        " ! application/x-rtp",
                        " ! rtph265depay",
                        // " ! h265parse", // we might want to add this in the future to expand the compatibility, since it can transform the stream format
                        " ! capsfilter name={filter_name} caps=video/x-h265,profile={profile},stream-format=byte-stream,alignment=au",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph265pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    location = location,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    profile = "main",
                    rtp_tee_name = rtp_tee_name,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for Onvif Pipeline"
                ))
            }
        };

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for OnvifPipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
