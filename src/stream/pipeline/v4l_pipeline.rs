use crate::{
    stream::{gst::utils::wait_for_element_state, types::CaptureConfiguration},
    video::{
        types::{VideoEncodeType, VideoSourceType},
        video_source_local::find_h264_profile_for_device,
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PipelineGstreamerInterface, PipelineState, PIPELINE_FILTER_NAME, PIPELINE_SINK_TEE_NAME,
};

use anyhow::{anyhow, Context, Result};

use tracing::*;

use gst::prelude::*;

#[derive(Debug)]
pub struct V4lPipeline {
    pub state: PipelineState,
}

impl V4lPipeline {
    #[instrument(level = "debug")]
    pub fn try_new(
        pipeline_id: uuid::Uuid,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(configuration) => configuration,
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

        let device = video_source.device_path.as_str();
        let width = configuration.width;
        let height = configuration.height;
        let interval_numerator = configuration.frame_interval.numerator;
        let interval_denominator = configuration.frame_interval.denominator;

        let description = match &configuration.encode {
            VideoEncodeType::H264 => {
                let Some(profile) = find_h264_profile_for_device(device, &width, &height, &interval_numerator, &interval_denominator) else {
                    return Err(anyhow!(
                        "The device {device:#?} doesn't support any of our known H264 profiles"
                    ));
                };
                format!(
                    concat!(
                        "v4l2src device={device} do-timestamp=false",
                        " ! h264parse",
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au,profile={profile},width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=10 pt=96",
                        " ! tee name={sink_tee_name} allow-not-linked=true"
                    ),
                    device = device,
                    profile = profile,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"),
                    sink_tee_name = format!("{PIPELINE_SINK_TEE_NAME}-{pipeline_id}"),
                )
            }
            VideoEncodeType::Yuyv => {
                format!(
                    concat!(
                        "v4l2src device={device} do-timestamp=false",
                        " ! videoconvert",
                        " ! capsfilter name={filter_name} caps=video/x-raw,format=I420,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! rtpvrawpay pt=96",
                        " ! tee name={sink_tee_name} allow-not-linked=true"
                    ),
                    device = device,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"),
                    sink_tee_name = format!("{PIPELINE_SINK_TEE_NAME}-{pipeline_id}"),
                )
            }
            VideoEncodeType::Mjpg => {
                format!(
                    concat!(
                        "v4l2src device={device} do-timestamp=false",
                        " ! jpegparse",
                        " ! capsfilter name={filter_name} caps=image/jpeg,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! rtpjpegpay pt=96",
                        " ! tee name={sink_tee_name} allow-not-linked=true"
                    ),
                    device = device,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"),
                    sink_tee_name = format!("{PIPELINE_SINK_TEE_NAME}-{pipeline_id}"),
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for V4L2 Pipeline"
                ))
            }
        };

        debug!("pipeline_description: {description:#?}");

        let pipeline = gst::parse_launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for V4lPipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}

#[instrument(level = "debug")]
pub fn get_default_v4l2_h264_profile(
    device: &str,
    width: &u32,
    height: &u32,
    interval_numerator: &u32,
    interval_denominator: &u32,
) -> Result<String> {
    // Here we are getting the profile chosen by gstreamer because if we don't set it in the capsfilter
    // before it is set to playing, when the second Sink connects to it, the pipeline will try to
    // renegotiate this capability, freezing the already-playing sinks for a while.

    if let Err(error) = gst::init() {
        error!("Error! {error}");
    };

    let description = format!(
        concat!(
            "v4l2src device={device} do-timestamp=false num-buffers=1",
            " ! h264parse ",
            " ! capsfilter name=Filter caps=video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_numerator}/{interval_denominator}",
            " ! fakesink",
        ),
        device = device,
        width = width,
        height = height,
        interval_denominator = interval_denominator,
        interval_numerator = interval_numerator,
    );

    let pipeline = gst::parse_launch(&description)?
        .downcast::<gst::Pipeline>()
        .expect("Couldn't downcast pipeline");

    debug!("Starting...");
    pipeline
        .set_state(gst::State::Playing)
        .expect("Unable to set the pipeline to the `Playing` state");
    if let Err(error) = wait_for_element_state(
        pipeline.upcast_ref::<gst::Element>(),
        gst::State::Playing,
        100,
        2,
    ) {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(anyhow!(
            "Failed setting Pipeline state to Playing to get H264 Profile. Reason: {error:?}"
        ));
    }

    debug!("Getting current profile...");
    let profile = pipeline
        .by_name("Filter")
        .context("Failed to access/find capsfilter element named \"Filter\"")?
        .static_pad("src")
        .context("Failed to access static src pad from capsfilter")?
        .current_caps()
        .context("capsfilter with a srcpad without any caps")?
        .iter()
        .find_map(|structure| {
            structure.iter().find_map(|(key, sendvalue)| {
                if key == "profile" {
                    Some(sendvalue.to_value().get::<String>().unwrap())
                } else {
                    None
                }
            })
        })
        .context(
            "Coudn't find any field called \"profile\" in srcpad caps of the capsfilter element",
        )?;

    debug!("Finishing...");
    pipeline
        .set_state(gst::State::Null)
        .context("Unable to set the pipeline to the `Null` state")?;
    if let Err(error) = wait_for_element_state(
        pipeline.upcast_ref::<gst::Element>(),
        gst::State::Null,
        100,
        2,
    ) {
        return Err(anyhow!(
            "Failed setting Pipeline to Null state. Reason: {error:?}"
        ));
    }

    Ok(profile)
}
