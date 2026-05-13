use std::sync::Arc;

use anyhow::{Result, anyhow};
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
    PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME,
    PipelineGstreamerInterface, PipelineState,
};

#[derive(Debug)]
pub struct OnvifPipeline {
    pub state: PipelineState,
}

impl OnvifPipeline {
    #[instrument(level = "debug", skip_all)]
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
                ));
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Onvif(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as V4l Pipeline"
                ));
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
        let raw_rtp_tee_name = format!("RawRtpTee-{pipeline_id}");

        let description = match encode {
            Some(VideoEncodeType::H264) => {
                format!(
                    concat!(
                        "rtspsrc location={location} is-live=true latency=0 buffer-mode=none do-retransmission=false udp-buffer-size=2621440",
                        " ! application/x-rtp, media=(string)video",
                        " ! tee name={raw_rtp_tee} allow-not-linked=true",
                        " {raw_rtp_tee}.",
                        " ! rtph264depay source-info=true",
                        " ! queue leaky=downstream silent=true flush-on-eos=true max-size-buffers=1 max-size-bytes=0 max-size-time=0",
                        " ! h264parse config-interval=-1",
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " {raw_rtp_tee}.",
                        " ! rtph264depay source-info=true",
                        " ! h264parse config-interval=-1",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=-1 source-info=true perfect-rtptime=false pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    location = location,
                    raw_rtp_tee = raw_rtp_tee_name,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            Some(VideoEncodeType::H265) => {
                format!(
                    concat!(
                        "rtspsrc location={location} is-live=true latency=0 buffer-mode=none do-retransmission=false udp-buffer-size=2621440",
                        " ! application/x-rtp, media=(string)video",
                        " ! tee name={raw_rtp_tee} allow-not-linked=true",
                        " {raw_rtp_tee}.",
                        " ! rtph265depay source-info=true",
                        " ! queue leaky=downstream silent=true flush-on-eos=true max-size-buffers=1 max-size-bytes=0 max-size-time=0",
                        " ! h265parse config-interval=-1",
                        " ! capsfilter name={filter_name} caps=video/x-h265,stream-format=byte-stream,alignment=au",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " {raw_rtp_tee}.",
                        " ! rtph265depay source-info=true",
                        " ! h265parse config-interval=-1",
                        " ! rtph265pay aggregate-mode=zero-latency config-interval=-1 source-info=true perfect-rtptime=false pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    location = location,
                    raw_rtp_tee = raw_rtp_tee_name,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for Onvif Pipeline"
                ));
            }
        };

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        pipeline.set_property("name", format!("pipeline-onvif-{pipeline_id}"));

        crate::stream::gst::utils::bypass_jitterbuffer(&pipeline);

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for OnvifPipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
