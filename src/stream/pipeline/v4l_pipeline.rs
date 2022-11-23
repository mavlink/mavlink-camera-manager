use crate::{
    stream::types::CaptureConfiguration,
    video::types::{VideoEncodeType, VideoSourceType},
    video_stream::types::VideoAndStreamInformation,
};

use super::pipeline::{PipelineGstreamerInterface, PipelineState, PIPELINE_TEE_NAME};

use anyhow::{bail, Result};

use tracing::*;

use gstreamer::prelude::*;

#[derive(Debug)]
pub struct V4lPipeline {
    pub state: PipelineState,
}
impl PipelineGstreamerInterface for V4lPipeline {
    fn build_pipeline(
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gstreamer::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::VIDEO(configuration) => configuration,
            unsupported => bail!("{unsupported:?} is not supported as V4l Pipeline"),
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Local(source) => source,
            unsupported => bail!("SourceType {unsupported:?} is not supported as V4l Pipeline"),
        };

        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                format!(concat!(
                    "v4l2src device={device} do-timestamp=false",
                    " ! h264parse",
                    " ! video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                    " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                    " ! tee name={tee_name} allow-not-linked=true"
                ),
                device = video_source.device_path,
                width = configuration.width,
                height = configuration.height,
                interval_denominator = configuration.frame_interval.denominator,
                interval_numerator = configuration.frame_interval.numerator,
                tee_name = PIPELINE_TEE_NAME
            )
            }
            unsupported => bail!("Encode {unsupported:?} is not supported for V4l Pipeline"),
        };

        debug!("pipeline_description: {description:#?}");

        let pipeline = gstreamer::parse_launch(&description)?;

        let pipeline = pipeline
            .downcast::<gstreamer::Pipeline>()
            .expect("Couldn't downcast pipeline");

        pipeline.debug_to_dot_file_with_ts(
            gstreamer::DebugGraphDetails::all(),
            "video_pipeline_created",
        );

        return Ok(pipeline);
    }

    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
