use anyhow::Result;

/// Interface to connect to WebRTCBin. It mimics the [RTCPeerConnection](https://developer.mozilla.org/en-US/docs/Web/API/RTCPeerConnection), available for browsers.
pub trait WebRTCBinInterface {
    /// Signal emitted when this WebRTCBin is ready to do negotiation to setup a WebRTC connection, when its state changes to PLAYING.
    ///
    /// * `element` - the original webrtc bin that had the callback attached to.
    fn on_negotiation_needed(&self, webrtcbin: &gst::Element) -> Result<()>;

    /// Signal emitted when this WebRTCBin creates an SDP `answer`.
    fn on_answer_created(
        &self,
        webrtcbin: &gst::Element,
        answer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()>;

    /// Signal emitted when this WebRTCBin creates an SDP `offer`.
    fn on_offer_created(
        &self,
        webrtcbin: &gst::Element,
        offer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()>;

    /// Signal emitted when this WebRTCBin creates its ICE candidates.
    /// * `sdp_m_line_index` - the zero-based index of the m-line attribute within the SDP to which the candidate should be associated to.
    /// * `candidate` - the ICE candidate.
    fn on_ice_candidate(
        &self,
        webrtcbin: &gst::Element,
        sdp_m_line_index: &u32,
        candidate: &str,
    ) -> Result<()>;

    /// Signal emitted when this WebRTCBin ICE gathering state changes.
    fn on_ice_gathering_state_change(
        &self,
        webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEGatheringState,
    );

    /// Signal emitted when this WebRTCBin ICE connection state changes.
    fn on_ice_connection_state_change(
        &self,
        webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEConnectionState,
    );

    /// Signal emitted when this WebRTCBin connection state changes.
    fn on_connection_state_change(
        &self,
        webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCPeerConnectionState,
    );

    /// Callback called by the WebRTC Signalling server (directly or by means of a session manager) for it to handle the incoming SDP.
    fn handle_sdp(&self, sdp: &gst_webrtc::WebRTCSessionDescription) -> Result<()>;

    /// Callback called by the WebRTC Signalling server (directly or by means of a session manager) for it to handle the incoming ICE candidate.
    fn handle_ice(&self, sdp_m_line_index: &u32, candidate: &str) -> Result<()>;
}
