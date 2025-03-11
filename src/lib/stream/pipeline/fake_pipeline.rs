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
pub struct FakePipeline {
    pub state: PipelineState,
}

impl FakePipeline {
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
                return Err(anyhow!("{unsupported:?} is not supported as Fake Pipeline"))
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "VideoSourceType {unsupported:?} is not supported as Fake Pipeline"
                ))
            }
        };

        let pattern = match &video_source.source {
            VideoSourceGstType::Fake(pattern) => pattern,
            unsupported => {
                return Err(anyhow!(
                    "VideoSourceGstType {unsupported:?} is not supported as Fake Pipeline"
                ))
            }
        };

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        // Fakes (videotestsrc) are only "video/x-raw" or "video/x-bayer",
        // and to be able to encode it, we need to define an available
        // format for both its src the next element's sink pad.
        // We are choosing "UYVY" because it is compatible with the
        // application-rtp template capabilities.
        // For more information: https://gstreamer.freedesktop.org/documentation/additional/design/mediatype-video-raw.html?gi-language=c#formats
        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                #[cfg(target_os = "macos")]
                let h264_encoder =
                    " ! vtenc_h264 allow-frame-reordering=false realtime=true bitrate=5000";

                // profile= is not supported by vtenc_h264
                #[cfg(target_os = "macos")]
                let capsfilter_profile = "";

                // "constrained-baseline" for windows and linux.
                #[cfg(not(target_os = "macos"))]
                let capsfilter_profile = ",profile=constrained-baseline";

                #[cfg(target_os = "windows")]
                let h264_encoder = " ! mfh264enc bitrate=5000";

                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let h264_encoder =
                    " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000";

                format!(concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format=I420",
                        "{h264_encoder}",
                        " ! h264parse",
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}{profile}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    h264_encoder = h264_encoder,
                    pattern = pattern,
                    profile = capsfilter_profile,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::H265 => {
                #[cfg(target_os = "macos")]
                let h265_encoder = " ! vtenc_h265 allow-frame-reordering=false realtime=true quality=0.0 bitrate=5000";

                #[cfg(target_os = "windows")]
                let h265_encoder = " ! mfh265enc bitrate=5000";

                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let h265_encoder =
                    " ! x265enc tune=zerolatency speed-preset=ultrafast bitrate=5000";

                format!(concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format=I420",
                        "{h265_encoder}",
                        " ! h265parse",
                        " ! capsfilter name={filter_name} caps=video/x-h265,profile={profile},stream-format=byte-stream,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph265pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    h265_encoder = h265_encoder,
                    pattern = pattern,
                    profile = "main",
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Yuyv => {
                format!(
                    concat!(
                        // Because application-rtp templates doesn't accept "YUY2", we
                        // need to transcode it. We are arbitrarily chosing the closest
                        // format available ("UYVY").
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format=I420",
                        " ! capsfilter name={filter_name} caps=video/x-raw,format=I420,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpvrawpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    pattern = pattern,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Mjpg => {
                format!(
                    concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format=I420",
                        " ! jpegenc quality=85 idct-method=1",
                        " ! capsfilter name={filter_name} caps=image/jpeg,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpjpegpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    pattern = pattern,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
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

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for FakePipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
