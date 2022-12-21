use crate::{
    stream::types::CaptureConfiguration, video::types::VideoSourceType,
    video_stream::types::VideoAndStreamInformation,
};

use super::pipeline::{PipelineGstreamerInterface, PipelineState, PIPELINE_TEE_NAME};

use anyhow::{anyhow, Context, Result};

use gst::prelude::*;

#[derive(Debug)]
pub struct RedirectPipeline {
    pub state: PipelineState,
}

impl RedirectPipeline {
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::REDIRECT(configuration) => configuration,
            unsupported => {
                return Err(anyhow!(
                    "{unsupported:?} is not supported as Redirect Pipeline"
                ))
            }
        };

        match &video_and_stream_information.video_source {
            VideoSourceType::Redirect(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as V4l Pipeline"
                ))
            }
        };

        if video_and_stream_information
            .stream_information
            .endpoints
            .len()
            > 1
        {
            return Err(anyhow!("Redirect must only have one endpoint"));
        }
        let url = &video_and_stream_information
            .stream_information
            .endpoints
            .first()
            .context("Failed to access the fisrt endpoint")?;

        let description = match url.scheme() {
            "rtsp" => {
                format!(
                    concat!(
                        "rtspsrc location={location} is-live=true latency=0",
                        " ! application/x-rtp",
                        " ! tee name={tee_name} allow-not-linked=true"
                    ),
                    location = url,
                    tee_name = PIPELINE_TEE_NAME
                )
            }
            "udp" => {
                format!(
                    concat!(
                        "udpsrc address={address} port={port} close-socket=false auto-multicast=true",
                        " ! application/x-rtp",
                        " ! tee name={tee_name} allow-not-linked=true"
                    ),
                    address = url.host().unwrap(),
                    port = url.port().unwrap(),
                    tee_name = PIPELINE_TEE_NAME
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Scheme {unsupported:#?} is not supported for Redirect Pipelines"
                ))
            }
        };

        let pipeline = gst::parse_launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for RedirectPipeline {
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
