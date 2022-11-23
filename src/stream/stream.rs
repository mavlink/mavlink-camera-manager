use super::pipeline::pipeline::Pipeline;
use super::sink::sink::Sink;
use super::sink::udp_sink::UdpSink;
use super::types::*;
use crate::mavlink::mavlink_camera::MavlinkCameraHandle;
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video_stream::types::VideoAndStreamInformation;

use anyhow::{anyhow, bail, Result};

use tracing::*;

#[allow(dead_code)]
pub struct Stream {
    pub pipeline: Pipeline,
    pub video_and_stream_information: VideoAndStreamInformation,
    pub mavlink_camera: Option<MavlinkCameraHandle>,
}

impl Stream {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        check_endpoints(video_and_stream_information)?;
        check_scheme(video_and_stream_information)?;

        let mut stream = create_stream(video_and_stream_information)?;

        if let VideoSourceType::Redirect(_) = &video_and_stream_information.video_source {
            // Do not add any Sink if it's a redirect Pipeline
            return Ok(stream);
        }

        // Add the desired Sinks to the Stream
        let mut any_error: Result<()> = Ok(());
        video_and_stream_information
            .stream_information
            .endpoints
            .iter()
            .for_each(|endpoint| {
                let endpoint = endpoint.scheme();
                let result = match endpoint {
                    "udp" => create_udp_sink(video_and_stream_information),
                    unsupported => Err(anyhow!("Unsupported Endpoint scheme: {unsupported}")),
                };

                if let Err(reason) = result.and_then(|sink| stream.pipeline.add_sink(sink)) {
                    let error = anyhow!(
                        "Failed to add Sink of type {endpoint} to the Pipeline. Reason: {reason}"
                    );
                    error!("{error:#?}");
                    any_error = Err(error);
                }
            });
        any_error?;

        Ok(stream)
    }
}

#[instrument(level = "debug")]
fn check_endpoints(video_and_stream_information: &VideoAndStreamInformation) -> Result<()> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints.is_empty() {
        bail!("Endpoints are empty")
    }

    Ok(())
}

#[instrument(level = "debug")]
fn check_scheme(video_and_stream_information: &VideoAndStreamInformation) -> Result<()> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;
    let encode = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::VIDEO(configuration) => configuration.encode.clone(),
        CaptureConfiguration::REDIRECT(_) => VideoEncodeType::UNKNOWN("".into()),
    };
    let scheme = endpoints.first().unwrap().scheme();

    if matches!(
        video_and_stream_information.video_source,
        VideoSourceType::Redirect(_)
    ) {
        match scheme {
            "udp" | "rtsp" => return Ok(()),
            _ => bail!(
                "The URL's scheme for REDIRECT endpoints should be \"udp\" or \"rtsp\", but was: {scheme:?}",
            )
        };
    }

    match scheme {
        "udp" => {
            if VideoEncodeType::H265 == encode {
                bail!("Endpoint with udp scheme only supports H264, encode type is H265, the scheme should be udp265.")
            }

            // UDP endpoints should contain both host and port
            if endpoints
                .iter()
                .any(|endpoint| endpoint.host().is_none() || endpoint.port().is_none())
            {
                bail!(
                    "Endpoint with udp scheme should contain host and port. Endpoints: {endpoints:#?}"
                )
            }
        }
        "udp265" => {
            if VideoEncodeType::H265 != encode {
                bail!("Endpoint with udp265 scheme only supports H265 encode. Encode: {encode:?}, Endpoints: {endpoints:#?}")
            }
        }
        _ => bail!("Scheme is not accepted as stream endpoint: {scheme}",),
    }

    return Ok(());
}

#[instrument(level = "debug")]
fn create_udp_sink(video_and_stream_information: &VideoAndStreamInformation) -> Result<Sink> {
    let id = video_and_stream_information.name.clone();
    let addresses = video_and_stream_information
        .stream_information
        .endpoints
        .clone();

    Ok(Sink::Udp(UdpSink::try_new(id, addresses)?))
}

#[instrument(level = "debug")]
fn create_stream(video_and_stream_information: &VideoAndStreamInformation) -> Result<Stream> {
    let pipeline = Pipeline::try_new(video_and_stream_information)?;
    let mavlink_camera = MavlinkCameraHandle::try_new(video_and_stream_information);

    Ok(Stream {
        pipeline,
        video_and_stream_information: video_and_stream_information.clone(),
        mavlink_camera,
    })
}
