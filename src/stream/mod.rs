pub mod gst;
pub mod manager;
pub mod pipeline;
pub mod rtsp;
pub mod sink;
pub mod types;
pub mod webrtc;

use crate::mavlink::mavlink_camera::MavlinkCameraHandle;
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video_stream::types::VideoAndStreamInformation;

use manager::Manager;
use pipeline::Pipeline;
use sink::{create_image_sink, create_rtsp_sink, create_udp_sink};
use types::*;
use webrtc::signalling_protocol::PeerId;
use webrtc::signalling_server::StreamManagementInterface;

use anyhow::{anyhow, Result};

use tracing::*;

use self::rtsp::rtsp_server::RTSP_SERVER_PORT;

#[derive(Debug)]
pub struct Stream {
    pub id: PeerId,
    pub pipeline: Pipeline,
    pub video_and_stream_information: VideoAndStreamInformation,
    pub mavlink_camera: Option<MavlinkCameraHandle>,
}

impl Stream {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        if let Err(error) = validate_endpoints(video_and_stream_information) {
            return Err(anyhow!("Failed validating endpoints. Reason: {error:?}"));
        }

        let pipeline = Pipeline::try_new(video_and_stream_information)?;
        let mavlink_camera = MavlinkCameraHandle::try_new(video_and_stream_information);
        let id = pipeline.inner_state_as_ref().pipeline_id;

        let mut stream = Stream {
            id,
            pipeline,
            video_and_stream_information: video_and_stream_information.clone(),
            mavlink_camera,
        };

        match &video_and_stream_information.video_source {
            VideoSourceType::Redirect(_) => return Ok(stream), // Do not add any Sink if it's a redirect Pipeline
            VideoSourceType::Gst(_) | VideoSourceType::Local(_) => (),
        }

        let endpoints = &video_and_stream_information.stream_information.endpoints;

        // Disable concurrent RTSP and UDP sinks creation, as it is failing.
        if endpoints.iter().any(|endpoint| endpoint.scheme() == "udp")
            && endpoints.iter().any(|endpoint| endpoint.scheme() == "rtsp")
        {
            return Err(anyhow!(
                "UDP endpoints won't work together with RTSP endpoints. You need to choose one. This is a (temporary) software limitation, if this is a feature you need, please, contact us."
            ));
        }

        if endpoints.iter().any(|endpoint| endpoint.scheme() == "udp") {
            if let Err(reason) =
                create_udp_sink(Manager::generate_uuid(), video_and_stream_information)
                    .and_then(|sink| stream.pipeline.add_sink(sink))
            {
                return Err(anyhow!(
                    "Failed to add Sink of type UDP to the Pipeline. Reason: {reason}"
                ));
            }
        }

        if endpoints.iter().any(|endpoint| endpoint.scheme() == "rtsp") {
            if let Err(reason) =
                create_rtsp_sink(Manager::generate_uuid(), video_and_stream_information)
                    .and_then(|sink| stream.pipeline.add_sink(sink))
            {
                return Err(anyhow!(
                    "Failed to add Sink of type RTSP to the Pipeline. Reason: {reason}"
                ));
            }
        }

        if let Err(reason) =
            create_image_sink(Manager::generate_uuid(), video_and_stream_information)
                .and_then(|sink| stream.pipeline.add_sink(sink))
        {
            return Err(anyhow!(
                "Failed to add Sink of type Image to the Pipeline. Reason: {reason}"
            ));
        }

        Ok(stream)
    }
}

#[instrument(level = "debug")]
fn validate_endpoints(video_and_stream_information: &VideoAndStreamInformation) -> Result<()> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints.is_empty() {
        return Err(anyhow!("Endpoints are empty"));
    }

    if endpoints.iter().filter(|&e| e.scheme() == "rtsp").count() > 1 {
        return Err(anyhow!("Only one RTSP endpoint is supported at time"));
    }

    let encode = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::Video(configuration) => configuration.encode.clone(),
        CaptureConfiguration::Redirect(_) => VideoEncodeType::Unknown("".into()),
    };

    let errors: Vec<anyhow::Error> = endpoints.iter().filter_map(|endpoint| {

        let scheme = endpoint.scheme();

        if matches!(
            video_and_stream_information.video_source,
            VideoSourceType::Redirect(_)
        ) {
            match scheme {
                "udp" | "rtsp" => return None,
                _ => return Some(anyhow!(
                    "The URL's scheme for REDIRECT endpoints should be \"udp\" or \"rtsp\", but was: {scheme:?}",
                ))
            };
        }

        match scheme {
            "udp" => {
                if VideoEncodeType::H265 == encode {
                    return Some(anyhow!("Endpoint with udp scheme only supports H264, encode type is H265, the scheme should be udp265."));
                }

                // UDP endpoints should contain both host and port
                if endpoint.host().is_none() || endpoint.port().is_none()
                {
                    return Some(anyhow!(
                        "Endpoint with udp scheme should contain host and port. Endpoint: {endpoint:?}"
                    ));
                }
            }
            "udp265" => {
                if VideoEncodeType::H265 != encode {
                    return Some(anyhow!("Endpoint with udp265 scheme only supports H265 encode. Encode: {encode:?}, Endpoint: {endpoints:?}"));
                }
            }
            "rtsp" => {
                // RTSP endpoints should contain host, port, and path
                if endpoint.host().is_none() || endpoint.port().is_none() || endpoint.path().is_empty() {
                    return Some(anyhow!(
                        "Endpoint with rtsp scheme should contain host, port, and path. Endpoint: {endpoint:?}"
                    ));
                }
                if endpoint.port() != Some(RTSP_SERVER_PORT) {
                    return Some(anyhow!(
                        "Endpoint with rtsp scheme should use port {RTSP_SERVER_PORT:?}. Endpoint: {endpoint:?}"
                    ));
                }
            }
            _ => {
                return Some(anyhow!(
                    "Scheme is not accepted as stream endpoint: {scheme}"
                ));
            }
        }

        None
    }).collect();

    if !errors.is_empty() {
        return Err(anyhow!(
            "One or more endpoints are invalid. List of Errors:\n{errors:?}",
        ));
    }

    Ok(())
}
