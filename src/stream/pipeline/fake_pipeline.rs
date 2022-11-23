use crate::{
    stream::types::CaptureConfiguration,
    video::{
        types::{VideoEncodeType, VideoSourceType},
        video_source_gst::VideoSourceGstType,
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::pipeline::{PipelineGstreamerInterface, PipelineState, PIPELINE_TEE_NAME};

use anyhow::{bail, Result};

use gstreamer::prelude::*;

#[derive(Debug)]
pub struct FakePipeline {
    pub state: PipelineState,
}
impl PipelineGstreamerInterface for FakePipeline {
    fn build_pipeline(
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gstreamer::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::VIDEO(configuration) => configuration,
            unsupported => bail!("{unsupported:?} is not supported as Fake Pipeline"),
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(source) => source,
            unsupported => {
                bail!("SourceType {unsupported:?} is not supported as Fake Pipeline")
            }
        };

        let pattern = match &video_source.source {
            VideoSourceGstType::Fake(pattern) => pattern,
            unsupported => {
                bail!("SourceType {unsupported:?} is not supported as Fake Pipeline")
            }
        };

        // Fakes (videotestsrc) are only "video/x-raw" or "video/x-bayer",
        // and to be able to encode it, we need to define an available
        // format for both its src the next element's sink pad.
        // We are choosing "UYVY" because it is compatible by the
        // application-rtp template capabilities.
        // For more information: https://gstreamer.freedesktop.org/documentation/additional/design/mediatype-video-raw.html?gi-language=c#formats
        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                format!(concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! videoconvert",
                        " ! video/x-raw,format=I420",
                        " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                        " ! h264parse",
                        " ! video/x-h264,profile=baseline,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={tee_name} allow-not-linked=true"
                    ),
                    pattern = pattern,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    tee_name = PIPELINE_TEE_NAME
                )
            }
            unsupported => bail!("Encode {unsupported:?} is not supported for V4l Pipeline"),
        };

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
