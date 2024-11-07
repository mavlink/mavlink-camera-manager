use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tokio::sync::mpsc::{self, WeakUnboundedSender};
use tracing::*;

use crate::{
    cli,
    stream::webrtc::{
        signalling_protocol::{
            Answer, BindAnswer, EndSessionQuestion, IceNegotiation, MediaNegotiation, Message,
            Question, RTCIceCandidateInit, RTCSessionDescription, Sdp,
        },
        webrtcbin_interface::WebRTCBinInterface,
    },
};

use super::SinkInterface;

#[derive(Clone)]
pub struct WebRTCSinkWeakProxy {
    bind: BindAnswer,
    sender: WeakUnboundedSender<Result<Message>>,
}

#[derive(Debug)]
pub struct WebRTCSink {
    pub queue: gst::Element,
    pub webrtcbin: gst::Element,
    pub webrtcbin_sink_pad: gst::Pad,
    pub tee_src_pad: Option<gst::Pad>,
    pub bind: BindAnswer,
    /// MPSC channel's sender to send messages to the respective Websocket from Signaller server. Err can be used to end the WebSocket.
    pub sender: mpsc::UnboundedSender<Result<Message>>,
    pub end_reason: Option<String>,
}
impl SinkInterface for WebRTCSink {
    #[instrument(level = "debug", skip(self, pipeline))]
    fn link(
        self: &mut WebRTCSink,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        // Configure transceiver https://gstreamer.freedesktop.org/documentation/webrtclib/gstwebrtc-transceiver.html?gi-language=c
        let webrtcbin_sink_pad = &self.webrtcbin_sink_pad;
        let transceiver =
            webrtcbin_sink_pad.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
        transceiver.set_property(
            "direction",
            gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
        );
        transceiver.set_property("do-nack", true); // Enable retransmission (RFC4588)
        transceiver.set_property("fec-type", gst_webrtc::WebRTCFECType::None);

        // Link
        let sink_id = &self.get_id();

        // Set Tee's src pad
        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from WebRTCBin {sink_id} has already been configured"
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        // Block data flow to prevent any data before set Playing, which would cause an error
        let Some(tee_src_pad_data_blocker) = tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
        else {
            let msg =
                "Failed adding probe to Tee's src pad to block data before going to playing state"
                    .to_string();
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            return Err(anyhow!(msg));
        };

        // Add the Sink elements to the Pipeline
        let elements = &[&self.queue, &self.webrtcbin];
        if let Err(add_err) = pipeline.add_many(elements) {
            let msg = format!("Failed to add WebRTCSink's elements to the Pipeline: {add_err:?}");
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            return Err(anyhow!(msg));
        }

        // Link the queue's src pad to the Sink's sink pad
        let queue_src_pad = &self
            .queue
            .static_pad("src")
            .expect("No sink pad found on Queue");
        if let Err(link_err) = queue_src_pad.link(&self.webrtcbin_sink_pad) {
            let msg =
                format!("Failed to link Queue's src pad with WebRTCBin's sink pad: {link_err:?}");
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Link the new Tee's src pad to the queue's sink pad
        let queue_sink_pad = &self
            .queue
            .static_pad("sink")
            .expect("No src pad found on Queue");
        if let Err(link_err) = tee_src_pad.link(queue_sink_pad) {
            let msg = format!("Failed to link Tee's src pad with Queue's sink pad: {link_err:?}");
            error!(msg);

            if let Err(unlink_err) = queue_src_pad.unlink(&self.webrtcbin_sink_pad) {
                error!(
                    "Failed to unlink Queue's src pad from WebRTCBin's sink pad: {unlink_err:?}"
                );
            }

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }
            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Syncronize added and linked elements
        if let Err(sync_err) = pipeline.sync_children_states() {
            let msg = format!("Failed to synchronize children states: {sync_err:?}");
            error!(msg);

            if let Err(unlink_err) = queue_src_pad.unlink(&self.webrtcbin_sink_pad) {
                error!(
                    "Failed to unlink Queue's src pad from WebRTCBin's sink pad: {unlink_err:?}"
                );
            }

            if let Err(unlink_err) = tee_src_pad.unlink(queue_sink_pad) {
                error!("Failed to unlink Tee's src pad from Queue's sink pad: {unlink_err:?}");
            }

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Unblock data to go through this added Tee src pad
        tee_src_pad.remove_probe(tee_src_pad_data_blocker);

        // TODO: Workaround for bug: https://gitlab.freedesktop.org/gstreamer/gst-plugins-bad/-/issues/1539
        // Reasoning: because we are not receiving the Disconnected | Failed | Closed of WebRTCPeerConnectionState,
        // we are directly connecting to webrtcbin->transceiver->transport->connect_state_notify:
        // When the bug is solved, we should remove this code and use WebRTCPeerConnectionState instead.
        let weak_proxy = self.downgrade();
        let rtp_sender = transceiver
            .sender()
            .context("Failed getting transceiver's RTP sender element")?;
        rtp_sender.connect_notify(Some("transport"), move |rtp_sender, _pspec| {
            let transport = rtp_sender.property::<gst_webrtc::WebRTCDTLSTransport>("transport");

            let weak_proxy = weak_proxy.clone();
            transport.connect_state_notify(move |transport| {
                use gst_webrtc::WebRTCDTLSTransportState::*;

                let state = transport.state();
                debug!("DTLS Transport Connection changed to {state:#?}");
                match state {
                    Failed | Closed => {
                        if let Err(error) =
                            weak_proxy.terminate(format!("DTLS closed with: {state:?}"))
                        {
                            error!("Failed sending EndSessionQuestion: {error}");
                        }
                    }
                    _ => (),
                }
            });
        });

        Ok(())
    }

    #[instrument(level = "debug", skip(self, pipeline))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink Sink from a pipeline without a Tee src pad.");
            return Ok(());
        };

        // Block data flow to prevent any data from holding the Pipeline elements alive
        if tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
            .is_none()
        {
            warn!(
                "Failed adding probe to Tee's src pad to block data before going to playing state"
            );
        }

        // Unlink the Queue element from the source's pipeline Tee's src pad
        let queue_sink_pad = self
            .queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(unlink_err) = tee_src_pad.unlink(&queue_sink_pad) {
            warn!("Failed unlinking WebRTC's Queue element from Tee's src pad: {unlink_err:?}");
        }
        drop(queue_sink_pad);

        // Release Tee's src pad
        if let Some(parent) = tee_src_pad.parent_element() {
            parent.release_request_pad(tee_src_pad)
        }

        // Remove the Sink's elements from the Source's pipeline
        let elements = &[&self.queue, &self.webrtcbin];
        if let Err(remove_err) = pipeline.remove_many(elements) {
            warn!("Failed removing WebRTCBin's elements from pipeline: {remove_err:?}");
        }

        // Instead of setting each element individually to null, we are using a temporary
        // pipeline so we can post and EOS and set the state of the elements to null
        // It is important to send EOS to the queue, otherwise it can hang when setting its state to null.
        let pipeline = gst::Pipeline::new();
        pipeline.add_many(elements).unwrap();
        pipeline.post_message(::gst::message::Eos::new()).unwrap();
        pipeline.set_state(gst::State::Null).unwrap();

        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    fn get_id(&self) -> uuid::Uuid {
        self.bind.session_id
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available: WebRTC Sink should only be connected by means of its Signalling protocol."
        ))
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn eos(&self) {
        let webrtcbin_weak = self.webrtcbin.downgrade();
        if let Err(error) = std::thread::Builder::new()
            .name("EOS".to_string())
            .spawn(move || {
                let webrtcbin = webrtcbin_weak.upgrade().unwrap();
                if let Err(error) = webrtcbin.post_message(gst::message::Eos::new()) {
                    error!("Failed posting Eos message into Sink bus. Reason: {error:?}");
                }
            })
            .expect("Failed spawning EOS thread")
            .join()
        {
            error!(
                "EOS Thread Panicked with: {:?}",
                error.downcast_ref::<String>()
            );
        }
    }
}

impl WebRTCSink {
    #[instrument(level = "debug", skip(sender))]
    pub fn try_new(
        bind: BindAnswer,
        sender: mpsc::UnboundedSender<Result<Message>>,
    ) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("silent", true)
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        // Workaround to have a better name for the threads created by the WebRTCBin element
        let webrtcbin = std::thread::Builder::new()
            .name("WebRTCBin".to_string())
            .spawn(move || {
                gst::ElementFactory::make("webrtcbin")
                    .property_from_str("name", format!("webrtcbin-{}", bind.session_id).as_str())
                    .property("async-handling", true)
                    .property("bundle-policy", gst_webrtc::WebRTCBundlePolicy::MaxBundle) // https://webrtcstandards.info/sdp-bundle/
                    .property("latency", 0u32)
                    .property_from_str("stun-server", cli::manager::stun_server_address().as_str())
                    .build()
            })
            .expect("Failed spawning WebRTCBin thread")
            .join()
            .map_err(|e| anyhow!("{:?}", e.downcast_ref::<String>()))??;

        cli::manager::turn_server_addresses()
            .iter()
            .for_each(|turn_server| {
                debug!("Trying to add turn server: {turn_server:?}");
                if !webrtcbin.emit_by_name::<bool>("add-turn-server", &[&turn_server.as_str()]) {
                    warn!("Failed adding turn server {turn_server:?}");
                }
            });

        // Configure RTP
        let webrtcbin = webrtcbin.downcast::<gst::Bin>().unwrap();
        webrtcbin
            .iterate_elements()
            .filter(|e| e.name().starts_with("rtpbin"))
            .into_iter()
            .for_each(|res| {
                let Ok(rtp_bin) = res else { return };

                // Use the pipeline clock time. This will ensure that the timestamps from the source are correct.
                rtp_bin.set_property_from_str("ntp-time-source", "clock-time");

                // Here we configure the RTP storage size, so the retransmission can compensate for longer periods of frame losts
                rtp_bin.connect("new-storage", false, move |values| {
                    let _rtp_bin = values[0].get::<gst::Element>().expect("Invalid argument");
                    let storage = values[1].get::<gst::Element>().expect("Invalid argument");
                    let _session = values[2].get::<u32>().expect("Invalid argument");

                    let current_time_ns = storage.property::<u64>("size-time");

                    let new_time_ns = std::time::Duration::from_secs(1).as_nanos() as u64;
                    debug!(
                        "Seting RTP storage size to {new_time_ns:?} ns, was {current_time_ns:?} ns"
                    );
                    storage.set_property("size-time", new_time_ns);

                    None
                });
            });
        let webrtcbin = webrtcbin.upcast::<gst::Element>();

        let webrtcbin_sink_pad = webrtcbin
            .request_pad_simple("sink_%u")
            .context("Failed requesting sink pad for webrtcsink")?;

        sender.send(Ok(Message::from(Answer::StartSession(bind.clone()))))?;

        let this = WebRTCSink {
            queue,
            webrtcbin,
            webrtcbin_sink_pad,
            tee_src_pad: None,
            bind,
            sender,
            end_reason: None,
        };

        let (peer_connected_tx, peer_connected_rx) = std::sync::mpsc::channel::<()>();

        // End the stream if it doesn't complete the negotiation
        let weak_proxy = this.downgrade();
        std::thread::Builder::new()
            .name("FailSafeKiller".to_string())
            .spawn(move || {
                debug!("Waiting for peer to be connected within 10 seconds...");

                std::thread::sleep(std::time::Duration::from_secs(9));

                if peer_connected_rx.recv_timeout(std::time::Duration::from_secs(1)).is_ok() {
                    debug!("Peer connected. Disabling FailSafeKiller");
                    return;
                }

                warn!("WebRTCBin failed to negotiate under 10 seconds. Session will be killed immediatly to save resources");

                if let Err(error) = weak_proxy.terminate("WebRTC negotiation timeout".to_string()) {
                    error!("Failed sending EndSessionQuestion: {error}");
                }
            })
            .expect("Failed spawning FailSafeKiller thread");

        // Connect to on-negotiation-needed to handle sending an Offer
        let weak_proxy = this.downgrade();
        this.webrtcbin
            .connect("on-negotiation-needed", false, move |values| {
                let element = values[0].get::<gst::Element>().expect("Invalid argument");

                if let Err(error) = weak_proxy.on_negotiation_needed(&element) {
                    error!("Failed to negotiate: {error:?}");
                }

                None
            });

        // Whenever there is a new ICE candidate, send it to the peer
        let weak_proxy = this.downgrade();
        this.webrtcbin
            .connect("on-ice-candidate", false, move |values| {
                let element = values[0].get::<gst::Element>().expect("Invalid argument");
                let sdp_m_line_index = values[1].get::<u32>().expect("Invalid argument");
                let candidate = values[2].get::<String>().expect("Invalid argument");

                if let Err(error) =
                    weak_proxy.on_ice_candidate(&element, &sdp_m_line_index, &candidate)
                {
                    debug!("Failed to send ICE candidate: {error}");
                }

                None
            });

        let weak_proxy = this.downgrade();
        this.webrtcbin
            .connect_notify(Some("connection-state"), move |webrtcbin, _pspec| {
                let state =
                    webrtcbin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");

                if matches!(state, gst_webrtc::WebRTCPeerConnectionState::Connected) {
                    if let Err(error) = peer_connected_tx.send(()) {
                        error!("Failed to disable FailSafeKiller: {error:?}");
                    }
                }

                if let Err(error) = weak_proxy.on_connection_state_change(webrtcbin, &state) {
                    error!("Failed to processing connection-state: {error:?}");
                }
            });

        let weak_proxy = this.downgrade();
        this.webrtcbin
            .connect_notify(Some("ice-connection-state"), move |webrtcbin, _pspec| {
                let state = webrtcbin
                    .property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");

                if let Err(error) = weak_proxy.on_ice_connection_state_change(webrtcbin, &state) {
                    error!("Failed to processing ice-connection-state: {error:?}");
                }
            });

        let weak_proxy = this.downgrade();
        this.webrtcbin
            .connect_notify(Some("ice-gathering-state"), move |webrtcbin, _pspec| {
                let state = webrtcbin
                    .property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state");

                if let Err(error) = weak_proxy.on_ice_gathering_state_change(webrtcbin, &state) {
                    error!("Failed to processing ice-gathering-state: {error:?}");
                }
            });

        Ok(this)
    }

    #[instrument(level = "debug", skip(self))]
    fn downgrade(&self) -> WebRTCSinkWeakProxy {
        WebRTCSinkWeakProxy {
            bind: self.bind.clone(),
            sender: self.sender.downgrade(),
        }
    }

    #[instrument(level = "debug", skip(self, sdp))]
    pub fn handle_sdp(&self, sdp: &gst_webrtc::WebRTCSessionDescription) -> Result<()> {
        self.downgrade().handle_sdp(&self.webrtcbin, sdp)
    }

    #[instrument(level = "debug", skip(self))]
    pub fn handle_ice(&self, sdp_m_line_index: &u32, candidate: &str) -> Result<()> {
        self.downgrade()
            .handle_ice(&self.webrtcbin, sdp_m_line_index, candidate)
    }
}

impl WebRTCSinkWeakProxy {
    fn terminate(&self, reason: String) -> Result<()> {
        let Some(sender) = self.sender.upgrade() else {
            return Err(anyhow!("Failed accessing MPSC Sender"));
        };

        if !sender.is_closed() {
            sender.send(Ok(Message::Question(Question::EndSession(
                EndSessionQuestion {
                    bind: self.bind.clone(),
                    reason,
                },
            ))))?;
        }

        Ok(())
    }
}

impl WebRTCBinInterface for WebRTCSinkWeakProxy {
    // Whenever webrtcbin tells us that (re-)negotiation is needed, simply ask
    // for a new offer SDP from webrtcbin without any customization and then
    // asynchronously send it to the peer via the WebSocket connection
    #[instrument(level = "debug", skip(self, webrtcbin))]
    fn on_negotiation_needed(&self, webrtcbin: &gst::Element) -> Result<()> {
        let this = self.clone();
        let webrtcbin_weak = webrtcbin.downgrade();
        let promise = gst::Promise::with_change_func(move |reply| {
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    error!("Offer creation future got no response");
                    return;
                }
                Err(error) => {
                    error!("Failed to send SDP offer: {error:?}");
                    return;
                }
            };

            let offer = match reply.get_optional::<gst_webrtc::WebRTCSessionDescription>("offer") {
                Ok(Some(offer)) => offer,
                Ok(None) => {
                    error!("Response got no \"offer\"");
                    return;
                }
                Err(error) => {
                    error!("Failed to send SDP offer: {error:?}");
                    return;
                }
            };

            if let Some(webrtcbin) = webrtcbin_weak.upgrade() {
                if let Err(error) = this.on_offer_created(&webrtcbin, &offer) {
                    error!("Failed to send SDP offer: {error}");
                }
            }
        });

        webrtcbin.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);

        Ok(())
    }

    // Once webrtcbin has create the offer SDP for us, handle it by sending it to the peer via the
    // WebSocket connection
    #[instrument(level = "debug", skip_all)]
    fn on_offer_created(
        &self,
        webrtcbin: &gst::Element,
        offer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        // Recreate the SDP offer with our customized SDP
        let offer = gst_webrtc::WebRTCSessionDescription::new(
            offer.type_(),
            customize_sent_sdp(&offer.sdp())?,
        );

        let Ok(sdp) = offer.sdp().as_text() else {
            return Err(anyhow!("Failed reading the created SDP"));
        };

        // All good, then set local description
        webrtcbin.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

        debug!("Sending SDP offer to peer. Offer:\n{sdp}");

        let message = MediaNegotiation {
            bind: self.bind.clone(),
            sdp: RTCSessionDescription::Offer(Sdp { sdp }),
        }
        .into();

        self.sender
            .upgrade()
            .context("Failed accessing MPSC Sender")?
            .send(Ok(message))?;

        Ok(())
    }

    // Once webrtcbin has create the answer SDP for us, handle it by sending it to the peer via the
    // WebSocket connection
    #[instrument(level = "debug", skip(self, _webrtcbin))]
    fn on_answer_created(
        &self,
        _webrtcbin: &gst::Element,
        answer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        // Recreate the SDP answer with our customized SDP
        let answer = gst_webrtc::WebRTCSessionDescription::new(
            answer.type_(),
            customize_sent_sdp(&answer.sdp())?,
        );

        let Ok(sdp) = answer.sdp().as_text() else {
            return Err(anyhow!("Failed reading the answer SDP"));
        };

        debug!("Sending SDP answer to peer. Answer:\n{sdp}");

        // All good, then set local description
        let message = MediaNegotiation {
            bind: self.bind.clone(),
            sdp: RTCSessionDescription::Answer(Sdp { sdp }),
        }
        .into();

        self.sender
            .upgrade()
            .context("Failed accessing MPSC Sender")?
            .send(Ok(message))
            .context("Failed to send SDP answer")?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self, _webrtcbin))]
    fn on_ice_candidate(
        &self,
        _webrtcbin: &gst::Element,
        sdp_m_line_index: &u32,
        candidate: &str,
    ) -> Result<()> {
        let message = IceNegotiation {
            bind: self.bind.clone(),
            ice: RTCIceCandidateInit {
                candidate: Some(candidate.to_owned()),
                sdp_mid: None,
                sdp_m_line_index: Some(sdp_m_line_index.to_owned()),
                username_fragment: None,
            },
        }
        .into();

        self.sender
            .upgrade()
            .context("Failed accessing MPSC Sender")?
            .send(Ok(message))?;

        debug!("ICE candidate created!");

        Ok(())
    }

    #[instrument(level = "debug", skip(self, _webrtcbin))]
    fn on_ice_gathering_state_change(
        &self,
        _webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEGatheringState,
    ) -> Result<()> {
        if let gst_webrtc::WebRTCICEGatheringState::Complete = state {
            debug!("ICE gathering complete")
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self, webrtcbin))]
    fn on_ice_connection_state_change(
        &self,
        webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEConnectionState,
    ) -> Result<()> {
        use gst_webrtc::WebRTCICEConnectionState::*;

        debug!("ICE connection changed to {state:#?}");
        match state {
            Completed => {
                let srcpads = webrtcbin.src_pads();
                if let Some(srcpad) = srcpads.first() {
                    srcpad.send_event(
                        gst_video::UpstreamForceKeyUnitEvent::builder()
                            .all_headers(true)
                            .build(),
                    );
                }
            }
            Failed | Closed | Disconnected => {
                self.terminate(format!("ICE closed with: {state:?}"))?;
            }
            _ => (),
        };

        Ok(())
    }

    #[instrument(level = "debug", skip(self, _webrtcbin))]
    fn on_connection_state_change(
        &self,
        _webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCPeerConnectionState,
    ) -> Result<()> {
        use gst_webrtc::WebRTCPeerConnectionState::*;

        debug!("Connection changed to {state:#?}");
        match state {
            // TODO: This would be the desired workflow, but it is not being detected, so we are using a workaround connecting direcly to the DTLS Transport connection state in the Session constructor.
            Disconnected | Failed | Closed => {
                self.terminate(format!("Connectiong closed with: {state:?}"))?;
            }
            _ => (),
        }

        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    fn handle_sdp(
        &self,
        webrtcbin: &gst::Element,
        sdp: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        let remote_sdp = webrtcbin
            .property::<Option<gst_webrtc::WebRTCSessionDescription>>("remote-description");

        if let Ok(sdp_str) = sdp.sdp().as_text() {
            trace!("Received SDP (type: {}):\n{sdp_str}", sdp.type_());
        };

        // This avoids a negotiation loop when the browser doesn't accept the SDP we sent
        if let Some(remote_sdp) = remote_sdp {
            if gst_webrtc::WebRTCSDPType::Answer == remote_sdp.type_()
                && remote_sdp.type_() == sdp.type_()
            {
                debug!("Skipping SDP because this session already has an SDP answer");

                return Ok(());
            }
        }

        let sdp = gst_webrtc::WebRTCSessionDescription::new(sdp.type_(), sanitize_sdp(&sdp.sdp())?);

        if let Ok(sdp_str) = sdp.sdp().as_text() {
            debug!(
                "Received SDP (Sanitized) (type: {}):\n{sdp_str}",
                sdp.type_()
            );
        };

        webrtcbin.emit_by_name::<()>("set-remote-description", &[&sdp, &None::<gst::Promise>]);
        Ok(())
    }

    #[instrument(level = "debug", skip(self, webrtcbin))]
    fn handle_ice(
        &self,
        webrtcbin: &gst::Element,
        sdp_m_line_index: &u32,
        candidate: &str,
    ) -> Result<()> {
        webrtcbin.emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
        Ok(())
    }
}

/// Because GStreamer's WebRTCBin often crashes when receiving an invalid SDP,
/// we use Mozzila's SDP parser to manipulate the SDP Message before giving it to GStreamer
#[instrument(level = "debug", skip_all)]
fn sanitize_sdp(sdp: &gst_sdp::SDPMessage) -> Result<gst_sdp::SDPMessage> {
    gst_sdp::SDPMessage::parse_buffer(
        webrtc_sdp::parse_sdp(sdp.as_text()?.as_str(), false)?
            .to_string()
            .as_bytes(),
    )
    .map_err(anyhow::Error::msg)
}

#[instrument(level = "debug", skip_all)]
fn customize_sent_sdp(sdp: &gst_sdp::SDPMessage) -> Result<gst_sdp::SDPMessage> {
    let mut new_sdp = sdp.clone();

    trace!("SDP: {:?}", new_sdp.as_text());

    new_sdp.medias_mut().enumerate().for_each(|(media_idx, media)| {
        let old_media = sdp.media(media_idx as u32).unwrap();

        old_media.attributes().for_each(|attribute| {
            if attribute.key().ne("rtpmap") {
                return;
            }

            let value = attribute.value().unwrap_or_default();

            trace!("Found a rtpmap attribute w/ value: {value:?}");

            lazy_static! {
                // Looking for something like "96 H264/90000"
                static ref RE: regex::Regex = regex::Regex::new(
                    r"(?P<payload>[0-9]*)\s(?P<encoding>[0-9A-Za-z_]{4})/(?P<clockrate>[0-9]*)"
                )
                .unwrap();
            }

            let Some(caps) = RE.captures(value) else {
                return;
            };
            let payload = &caps["payload"];
            let encoding = &caps["encoding"];
            let clockrate = &caps["clockrate"];

            trace!("rtpmap attribute parsed: payload: {payload:?}, encoding: {encoding:?}, clockrate: {clockrate:?}");

            if let Some((fmtp_idx, fmtp_attribute)) =
                old_media.attributes().enumerate().find(|(_, attribute)| {
                    attribute.key().eq("fmtp")
                        && attribute
                            .value()
                            .map(|v| v.starts_with(payload))
                            .unwrap_or(false)
                })
            {
                let value = fmtp_attribute
                .value()
                .expect("The fmtp we have found should have a value");

                trace!("Found a fmtp attribute: {value:?}");

                let Some((payload, configs_str)) = value.split_once(' ') else {
                    return;
                };

                let mut new_configs = configs_str.split(';').map(|v|v.to_string()).collect::<Vec<String>>();
                new_configs.retain(|v| {
                    v.starts_with("sprop-parameter-sets")
                });

                trace!("fmtp attribute parsed: payload: {payload:?}, values: {new_configs:?}");

                match encoding {
                    "H264" => {
                        // Reference: https://www.iana.org/assignments/media-types/video/H264
                        const CONSTRAINED_BASELINE_LEVEL_ID: u32 = 0x42e01f;
                        new_configs.push("packetization-mode=1".to_string());
                        new_configs.push(format!("profile-level-id={CONSTRAINED_BASELINE_LEVEL_ID:x}"));
                        new_configs.push("level-asymmetry-allowed=1".to_string());

                    }
                    "H265" => {
                        // Rererence: https://www.iana.org/assignments/media-types/video/H265
                        const LEVEL_ID: u8 = 93;
                        new_configs.push(format!("level-id={LEVEL_ID}"));

                    }
                    _ => (),
                }

                let new_configs_str = new_configs.join(";");
                let new_value = [payload, &new_configs_str].join(" ");

                let new_fmtp_attribute = gst_sdp::SDPAttribute::new("fmtp", Some(&new_value));

                if let Err(error) = media.replace_attribute(fmtp_idx as u32, new_fmtp_attribute) {
                    warn!("Failed to customize fmtp attribute \nfrom: {value:?}\nto: {new_value:?}.\nError: {error:?}");
                }

                trace!("fmtp attribute changed \nfrom: {value:?}\nto: {new_value:?}");
            }
        });
    });

    Ok(new_sdp)
}
