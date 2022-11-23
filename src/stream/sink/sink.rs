use enum_dispatch::enum_dispatch;

use super::udp_sink::UdpSink;

#[enum_dispatch]
pub trait SinkInterface {
    /// Returns the Sink element
    fn get_element(&self) -> std::sync::MutexGuard<'_, gstreamer::Element>;

    /// Returns the unique id of the Sink
    fn get_id(&self) -> &String;

    /// Returns the src pad from its Sink element
    fn get_sink_pad(&self) -> &gstreamer::Pad;

    /// Returns the associated Tee src pad
    fn get_tee_src_pad(&self) -> Option<&gstreamer::Pad>;

    /// Sets the associated Tee src pad
    fn set_tee_src_pad(self, tee_src_pad: gstreamer::Pad) -> Sink;
}

#[enum_dispatch(SinkInterface)]
#[derive(Debug)]
pub enum Sink {
    Udp(UdpSink),
    // Rtsp(RtspSink),
    // WebRTC(WebRTCSink),
}
