use crate::{
    stream::types::CaptureConfiguration,
    video::types::{VideoEncodeType, VideoSourceType},
    video_stream::types::VideoAndStreamInformation,
};

use super::pipeline::{
    PipelineGstreamerInterface, PipelineState, PIPELINE_FILTER_NAME, PIPELINE_TEE_NAME,
};

use anyhow::{anyhow, Result};

use tracing::*;

use gst::prelude::*;

#[derive(Debug)]
pub struct V4lPipeline {
    pub state: PipelineState,
}

impl V4lPipeline {
    #[instrument(level = "debug")]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::VIDEO(configuration) => configuration,
            unsupported => return Err(anyhow!("{unsupported:?} is not supported as V4l Pipeline")),
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Local(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as V4l Pipeline"
                ))
            }
        };

        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                format!(concat!(
                    "v4l2src device={device} do-timestamp=false",
                    " ! h264parse",
                    " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                    " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                    " ! tee name={tee_name} allow-not-linked=true"
                ),
                device = video_source.device_path,
                width = configuration.width,
                height = configuration.height,
                interval_denominator = configuration.frame_interval.denominator,
                interval_numerator = configuration.frame_interval.numerator,
                filter_name = PIPELINE_FILTER_NAME,
                tee_name = PIPELINE_TEE_NAME
            )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for V4l Pipeline"
                ))
            }
        };

        debug!("pipeline_description: {description:#?}");

        let pipeline = gst::parse_launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        return Ok(pipeline);
    }
}

impl PipelineGstreamerInterface for V4lPipeline {
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
