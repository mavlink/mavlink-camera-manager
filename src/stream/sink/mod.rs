pub mod image_sink;
pub mod rtsp_sink;
pub mod udp_sink;
pub mod webrtc_sink;

use enum_dispatch::enum_dispatch;

use crate::video_stream::types::VideoAndStreamInformation;

use image_sink::ImageSink;
use rtsp_sink::RtspSink;
use udp_sink::UdpSink;
use webrtc_sink::WebRTCSink;

use anyhow::{anyhow, Result};

use tracing::*;

#[enum_dispatch]
pub trait SinkInterface {
    /// Link this Sink's sink pad to the given Pipelines's Tee element's src pad.
    /// Read important notes about dynamically pipeline manipulation [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()>;

    /// Unlink this Sink's sink pad from the already associated Pipelines's Tee element's src pad.
    /// Read important notes about dynamically pipeline manipulation [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()>;

    /// Get the id associated with this Sink
    fn get_id(&self) -> uuid::Uuid;

    /// Get the sdp file describing this Sink, following the [RFC 8866](https://www.rfc-editor.org/rfc/rfc8866.html)
    ///
    /// For a better grasp of SDP parameters, read [here](https://www.iana.org/assignments/sdp-parameters/sdp-parameters.xhtml)
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage>;
}

#[enum_dispatch(SinkInterface)]
#[derive(Debug)]
pub enum Sink {
    Udp(UdpSink),
    Rtsp(RtspSink),
    WebRTC(WebRTCSink),
    Image(ImageSink),
}

#[instrument(level = "debug")]
pub fn create_udp_sink(
    id: uuid::Uuid,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    let addresses = video_and_stream_information
        .stream_information
        .endpoints
        .clone();

    Ok(Sink::Udp(UdpSink::try_new(id, addresses)?))
}

#[instrument(level = "debug")]
pub fn create_rtsp_sink(
    id: uuid::Uuid,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    let addresses = video_and_stream_information
        .stream_information
        .endpoints
        .clone();

    Ok(Sink::Rtsp(RtspSink::try_new(id, addresses)?))
}

#[instrument(level = "debug")]
pub fn create_image_sink(
    id: uuid::Uuid,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    let encoding = match &video_and_stream_information
        .stream_information
        .configuration
    {
        super::types::CaptureConfiguration::Video(video_configuraiton) => {
            video_configuraiton.encode.clone()
        }
        super::types::CaptureConfiguration::Redirect(_) => {
            return Err(anyhow!(
                "ImageSinks are not yet implemented for Redirect sources"
            ))
        }
    };
    Ok(Sink::Image(ImageSink::try_new(id, encoding)?))
}
