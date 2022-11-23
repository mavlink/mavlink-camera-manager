use enum_dispatch::enum_dispatch;

use crate::video_stream::types::VideoAndStreamInformation;

use super::{udp_sink::UdpSink, webrtc_sink::WebRTCSink};

use anyhow::Result;

use tracing::*;

#[enum_dispatch]
pub trait SinkInterface {
    /// Link this Sink's sink pad to the given Pipelines's Tee element's src pad.
    /// /// Read important notes about dynamically pipeline manipulation [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
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
}

#[enum_dispatch(SinkInterface)]
#[derive(Debug)]
pub enum Sink {
    Udp(UdpSink),
    WebRTC(WebRTCSink),
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
