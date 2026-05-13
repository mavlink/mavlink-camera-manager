use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tokio::sync::mpsc::{self, WeakUnboundedSender};
use tracing::*;

use crate::{
    cli,
    stream::gst::utils::wait_for_element_state,
    stream::webrtc::{
        signalling_protocol::{
            Answer, BindAnswer, EndSessionQuestion, IceNegotiation, MediaNegotiation, Message,
            Question, RTCIceCandidateInit, RTCSessionDescription, Sdp,
        },
        webrtcbin_interface::WebRTCBinInterface,
    },
};

use super::{SinkInterface, force_sync_false_on_element, link_sink_to_tee, unlink_sink_from_tee};

const PLAYOUT_DELAY_URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";
const PLAYOUT_DELAY_EXT_ID: u8 = 13;

#[derive(Clone)]
pub struct WebRTCSinkWeakProxy {
    bind: BindAnswer,
    sender: WeakUnboundedSender<Result<Message>>,
}

#[derive(Debug)]
pub struct WebRTCSink {
    pub queue: gst::Element,
    pub webrtcbin: gst::Element,
    pub webrtcbin_sink_pad: Option<gst::Pad>,
    pub tee_src_pad: Option<gst::Pad>,
    pub bind: BindAnswer,
    /// MPSC channel's sender to send messages to the respective Websocket from Signaller server. Err can be used to end the WebSocket.
    pub sender: mpsc::UnboundedSender<Result<Message>>,
    pub end_reason: Option<String>,
}

impl Drop for WebRTCSink {
    fn drop(&mut self) {
        if let Some(pad) = self.webrtcbin_sink_pad.take() {
            self.webrtcbin.release_request_pad(&pad);
        }
        let _ = self.webrtcbin.set_state(gst::State::Null);
        if let Err(error) =
            wait_for_element_state(self.webrtcbin.downgrade(), gst::State::Null, 100, 5)
        {
            warn!("webrtcbin did not reach Null within 5 s on drop: {error:?}");
        }
        let _ = self.queue.set_state(gst::State::Null);
        if let Err(error) = wait_for_element_state(self.queue.downgrade(), gst::State::Null, 100, 2)
        {
            warn!("queue did not reach Null within 2 s on drop: {error:?}");
        }
    }
}
impl SinkInterface for WebRTCSink {
    #[instrument(level = "debug", skip(self, pipeline))]
    fn link(
        self: &mut WebRTCSink,
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        // Configure transceiver https://gstreamer.freedesktop.org/documentation/webrtclib/gstwebrtc-transceiver.html?gi-language=c
        let webrtcbin_sink_pad = self
            .webrtcbin_sink_pad
            .as_ref()
            .context("webrtcbin_sink_pad already consumed")?;
        let transceiver =
            webrtcbin_sink_pad.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
        transceiver.set_property(
            "direction",
            gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
        );
        transceiver.set_property("do-nack", true); // Enable retransmission (RFC4588)
        transceiver.set_property("fec-type", gst_webrtc::WebRTCFECType::None);

        // Provide codec-preferences so webrtcbin can create an SDP offer
        // without waiting for buffer caps on the sink pad.  The queue src
        // pad carries a BLOCK probe that prevents buffers (and their
        // associated sticky caps) from reaching webrtcbin until the
        // RED/FEC/RTX encoders are excised on Connected.  Without
        // codec-preferences, on-negotiation-needed never fires because
        // webrtcbin defers negotiation until caps arrive.
        //
        // Walk upstream: tee sink pad → peer (payloader src) to discover
        // the encoding-name, then build fixed caps (webrtcbin needs caps
        // without ranges to produce a valid SDP media section).
        let codec_caps = tee_src_pad
            .parent_element()
            .and_then(|tee| tee.static_pad("sink"))
            .and_then(|tee_sink| {
                let negotiated = tee_sink.current_caps();
                let caps = negotiated.or_else(|| {
                    tee_sink.peer().and_then(|peer| {
                        let queried = peer.query_caps(None);
                        (!queried.is_any() && queried.size() > 0).then_some(queried)
                    })
                })?;

                let s = caps.structure(0)?;
                let encoding = s.get::<&str>("encoding-name").ok()?;
                let clock_rate = s.get::<i32>("clock-rate").unwrap_or(90000);
                let payload = s.get::<i32>("payload").unwrap_or(96);

                Some(
                    gst::Caps::builder("application/x-rtp")
                        .field("media", "video")
                        .field("encoding-name", encoding)
                        .field("clock-rate", clock_rate)
                        .field("payload", payload)
                        .build(),
                )
            });
        if let Some(caps) = codec_caps {
            debug!("Setting codec-preferences: {caps}");
            transceiver.set_property("codec-preferences", &caps);
        } else {
            warn!("No caps available upstream of tee for codec-preferences");
        }

        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from Sink {:?} has already been configured",
                self.get_id()
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        let elements = &[&self.queue, &self.webrtcbin];
        link_sink_to_tee(tee_src_pad, pipeline, elements)?;

        // Block data at the queue src pad so that no buffers reach webrtcbin's
        // internal RED/FEC/RTX encoders before they are excised on Connected.
        // On "warm" connections the tee already has data flowing; without this
        // block those buffers would pass through rtpredenc (payload type not in
        // the SDP), causing permanent codec resolution failure in the browser.
        // The block is implicitly cleaned up when the queue is excised.
        if let Some(queue_src_pad) = self.queue.static_pad("src") {
            if queue_src_pad
                .add_probe(
                    gst::PadProbeType::BLOCK
                        | gst::PadProbeType::BUFFER
                        | gst::PadProbeType::BUFFER_LIST,
                    |_pad, _info| gst::PadProbeReturn::Ok,
                )
                .is_some()
            {
                debug!("Installed pre-excision block on queue src pad");
            } else {
                warn!("Failed to install pre-excision block on queue src pad");
            }
        }

        install_playout_delay_probe(&self.queue);

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
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &Arc<uuid::Uuid>) -> Result<()> {
        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink Sink from a pipeline without a Tee src pad.");
            return Ok(());
        };

        let queue_in_pipeline = self.queue.parent().is_some();
        if queue_in_pipeline {
            let elements = &[&self.queue, &self.webrtcbin];
            unlink_sink_from_tee(tee_src_pad, pipeline, elements)?;
        } else {
            let elements = &[&self.webrtcbin];
            unlink_sink_from_tee(tee_src_pad, pipeline, elements)?;
        }

        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    fn get_id(&self) -> Arc<uuid::Uuid> {
        Arc::new(self.bind.session_id)
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "WebRTC Sink can only be connected via its Signalling protocol"
        ))
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn eos(&self) {
        // Intentionally a no-op.  `unlink_sink_from_tee` already sends
        // an EOS *event* directly to the webrtcbin element, and the
        // WebRTCSink Drop handler sets it to Null.  The previous
        // implementation used `post_message(Eos::new())` which posted
        // an EOS *message* to the pipeline bus, causing the bus watcher
        // to interpret it as a pipeline-level EOS and kill the runner.
    }

    fn pipeline(&self) -> Option<&gst::Pipeline> {
        None
    }
}

impl WebRTCSink {
    #[instrument(level = "debug", skip(sender))]
    pub fn try_new(
        bind: BindAnswer,
        sender: mpsc::UnboundedSender<Result<Message>>,
    ) -> Result<Self> {
        // Temporary queue between the tee and webrtcbin: excised on
        // Connected together with the internal RED/FEC/RTX encoders.
        // leaky=downstream + uncapped size limits ensure a 4K keyframe
        // burst can pass through without drops during the DTLS handshake.
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream")
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32)
            .property("max-size-bytes", 0u32)
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

        // Configure the underlying NiceAgent for robust ICE behaviour:
        //
        // - `keepalive-conncheck` (requires libnice >= 0.1.8): use real
        //   STUN connectivity checks as keepalives so ICE can detect when
        //   the peer stops responding.
        //
        // - `upnp` = false: disable UPnP/IGD port-mapping.  The gupnp-igd
        //   threads spawned for SSDP discovery have long network timeouts
        //   and linger after the NiceAgent is destroyed, causing thread
        //   leaks visible in stress tests.  STUN/TURN is used instead.
        {
            let ice: glib::Object = webrtcbin.property("ice-agent");
            if ice.find_property("agent").is_some() {
                let agent: glib::Object = ice.property("agent");
                if agent.find_property("keepalive-conncheck").is_some() {
                    agent.set_property("keepalive-conncheck", true);
                    debug!("Enabled ICE keepalive-conncheck for faster peer-loss detection");
                } else {
                    debug!("NiceAgent does not support keepalive-conncheck property");
                }
                if agent.find_property("upnp").is_some() {
                    agent.set_property("upnp", false);
                    debug!("Disabled UPnP/IGD on NiceAgent");
                }
            }
        }

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

                rtp_bin.connect("new-storage", false, move |values| {
                    let _rtp_bin = values[0].get::<gst::Element>().expect("Invalid argument");
                    let storage = values[1].get::<gst::Element>().expect("Invalid argument");
                    let _session = values[2].get::<u32>().expect("Invalid argument");

                    let current_time_ns = storage.property::<u64>("size-time");
                    debug!("Disabling RTP storage (was {current_time_ns} ns)");
                    storage.set_property("size-time", 0u64);

                    None
                });
            });
        let webrtcbin = webrtcbin.upcast::<gst::Element>();

        let webrtcbin_sink_pad = Some(
            webrtcbin
                .request_pad_simple("sink_%u")
                .context("Failed requesting sink pad for webrtcsink")?,
        );

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

        // End the stream if it doesn't complete the negotiation.
        // Uses recv_timeout so the thread exits immediately when:
        //  - the peer connects (Ok received),
        //  - the session is torn down (channel Disconnected), or
        //  - the 10-second timeout elapses (proceed to kill).
        let weak_proxy = this.downgrade();
        std::thread::Builder::new()
            .name("FailSafeKiller".to_string())
            .spawn(move || {
                debug!("Waiting for peer to be connected within 10 seconds...");

                match peer_connected_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                    Ok(()) => {
                        debug!("Peer connected. Disabling FailSafeKiller");
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        debug!("Session ended before negotiation timeout. FailSafeKiller exiting.");
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }

                warn!("WebRTC negotiation timed out (10s), killing session");

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
        let queue_weak = this.queue.downgrade();
        this.webrtcbin
            .connect_notify(Some("connection-state"), move |webrtcbin, _pspec| {
                let state =
                    webrtcbin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");

                if matches!(state, gst_webrtc::WebRTCPeerConnectionState::Connected) {
                    if let Err(error) = peer_connected_tx.send(()) {
                        error!("Failed to disable FailSafeKiller: {error:?}");
                    }

                    if let Some(queue) = queue_weak.upgrade() {
                        optimise_send_path(webrtcbin, &queue);
                    }

                    send_force_key_unit_upstream(webrtcbin, &queue_weak);
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

            if let Some(webrtcbin) = webrtcbin_weak.upgrade()
                && let Err(error) = this.on_offer_created(&webrtcbin, &offer)
            {
                error!("Failed to send SDP offer: {error}");
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
            customize_sent_sdp(offer.sdp())?,
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
            customize_sent_sdp(answer.sdp())?,
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
        if let Some(remote_sdp) = remote_sdp
            && gst_webrtc::WebRTCSDPType::Answer == remote_sdp.type_()
            && remote_sdp.type_() == sdp.type_()
        {
            debug!("Skipping SDP because this session already has an SDP answer");

            return Ok(());
        }

        let sdp = gst_webrtc::WebRTCSessionDescription::new(sdp.type_(), sanitize_sdp(sdp.sdp())?);

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
fn sanitize_sdp(sdp: &gst_sdp::SDPMessageRef) -> Result<gst_sdp::SDPMessage> {
    gst_sdp::SDPMessage::parse_buffer(
        webrtc_sdp::parse_sdp(sdp.as_text()?.as_str(), false)?
            .to_string()
            .as_bytes(),
    )
    .map_err(anyhow::Error::msg)
}

#[instrument(level = "debug", skip_all)]
fn customize_sent_sdp(sdp: &gst_sdp::SDPMessageRef) -> Result<gst_sdp::SDPMessage> {
    let mut new_sdp = sdp.to_owned();

    trace!("SDP: {:?}", new_sdp.as_text());

    new_sdp
        .medias_mut()
        .enumerate()
        .for_each(|(media_idx, media)| {
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

                trace!("rtpmap: pt={payload:?} enc={encoding:?} clk={clockrate:?}");

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

                    let mut new_configs = configs_str
                        .split(';')
                        .map(|v| v.to_string())
                        .collect::<Vec<String>>();
                    new_configs.retain(|v| {
                        v.starts_with("sprop-parameter-sets")
                            || v.starts_with("sprop-vps")
                            || v.starts_with("sprop-sps")
                            || v.starts_with("sprop-pps")
                    });

                    trace!("fmtp attribute parsed: payload: {payload:?}, values: {new_configs:?}");

                    match encoding {
                        "H264" => {
                            // Reference: https://www.iana.org/assignments/media-types/video/H264
                            let original_plid = configs_str
                                .split(';')
                                .find_map(|kv| kv.strip_prefix("profile-level-id="))
                                .unwrap_or("unknown");
                            let level = original_plid.get(4..6).unwrap_or("1f");
                            let new_plid = format!("42e0{level}");
                            new_configs.push("packetization-mode=1".to_string());
                            new_configs.push(format!("profile-level-id={new_plid}"));
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

                    if let Err(error) = media.replace_attribute(fmtp_idx as u32, new_fmtp_attribute)
                    {
                        warn!("fmtp customization failed: {error:?}");
                    }

                    trace!("fmtp attribute changed \nfrom: {value:?}\nto: {new_value:?}");
                }
            });
        });

    // Some SDP from RTSP cameras end up with a "a=recvonly" that breaks the webrtcbin when the browser responds, so we are removing them here
    new_sdp.medias_mut().for_each(|media| {
        let mut attributes_to_remove = media
            .attributes()
            .enumerate()
            .filter_map(|(attribute_idx, attribute)| {
                matches!(attribute.key(), "recvonly").then_some(attribute_idx)
            })
            .collect::<Vec<usize>>();
        attributes_to_remove.reverse();

        for attribute_idx in attributes_to_remove {
            let _ = media.remove_attribute(attribute_idx as u32);
        }
    });

    // Strip FEC (ulpfec) and RED payload types from the SDP offer.
    // Even though fec-type is set to None on the transceiver, webrtcbin
    // still creates rtpulpfecenc and rtpredenc elements internally. Removing
    // these codecs from the SDP prevents the peer from negotiating them,
    // which avoids unnecessary FEC/RED packet processing on both sides.
    new_sdp.medias_mut().for_each(|media| {
        strip_fec_and_red_from_media(media);
    });

    // Add playout-delay RTP header extension (min=0, max=0 = render immediately).
    new_sdp.medias_mut().for_each(|media| {
        let already_present = media.attributes().any(|a| {
            a.key() == "extmap"
                && a.value()
                    .map(|v| v.contains(PLAYOUT_DELAY_URI))
                    .unwrap_or(false)
        });
        if already_present {
            return;
        }

        let id_taken = media.attributes().any(|a| {
            a.key() == "extmap"
                && a.value()
                    .and_then(|v| v.split_whitespace().next()?.parse::<u8>().ok())
                    == Some(PLAYOUT_DELAY_EXT_ID)
        });
        if id_taken {
            warn!("Playout-delay: extmap ID {PLAYOUT_DELAY_EXT_ID} already in use, skipping");
            return;
        }

        let value = format!("{PLAYOUT_DELAY_EXT_ID} {PLAYOUT_DELAY_URI}");
        media.add_attribute("extmap", Some(&value));
        debug!("Added playout-delay extmap (ID {PLAYOUT_DELAY_EXT_ID}) to SDP offer");
    });

    Ok(new_sdp)
}

/// Remove RED and ULPFEC payload types from a single SDP media section.
///
/// Scans `a=rtpmap` attributes for encodings containing "red/" or "ulpfec/",
/// collects their payload-type numbers, then removes the corresponding
/// `a=rtpmap`, `a=fmtp`, and format-list entries.
fn strip_fec_and_red_from_media(media: &mut gst_sdp::SDPMediaRef) {
    let mut fec_red_pts: Vec<String> = Vec::new();

    for attr in media.attributes() {
        if attr.key() == "rtpmap"
            && let Some(value) = attr.value()
        {
            let lower = value.to_lowercase();
            if (lower.contains(" red/") || lower.contains(" ulpfec/"))
                && let Some(pt) = value.split_whitespace().next()
            {
                fec_red_pts.push(pt.to_string());
            }
        }
    }

    if fec_red_pts.is_empty() {
        return;
    }

    debug!("Stripping FEC/RED payload types from SDP: {fec_red_pts:?}");

    let mut attr_indices: Vec<usize> = Vec::new();
    for (idx, attr) in media.attributes().enumerate() {
        if matches!(attr.key(), "rtpmap" | "fmtp")
            && let Some(value) = attr.value()
            && let Some(pt) = value.split_whitespace().next()
            && fec_red_pts.iter().any(|p| p == pt)
        {
            attr_indices.push(idx);
        }
    }
    for idx in attr_indices.into_iter().rev() {
        let _ = media.remove_attribute(idx as u32);
    }

    let mut fmt_indices: Vec<u32> = Vec::new();
    for i in 0..media.formats_len() {
        if let Some(fmt) = media.format(i)
            && fec_red_pts.iter().any(|p| p == fmt)
        {
            fmt_indices.push(i);
        }
    }
    for idx in fmt_indices.into_iter().rev() {
        let _ = media.remove_format(idx);
    }
}

/// Install a BUFFER probe on the queue's src pad that writes the playout-delay
/// RTP header extension (min=0, max=0) into every outgoing RTP packet.
/// This tells the browser "render immediately, no smoothing buffer."
fn install_playout_delay_probe(queue: &gst::Element) {
    let Some(src_pad) = queue.static_pad("src") else {
        warn!("install_playout_delay_probe: queue has no src pad");
        return;
    };

    src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
        if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
            let buffer = buffer.make_mut();
            if let Ok(mut rtp) = gst_rtp::RTPBuffer::from_buffer_writable(buffer) {
                let _ = rtp.add_extension_onebyte_header(PLAYOUT_DELAY_EXT_ID, &[0x00, 0x00, 0x00]);
            }
        }
        gst::PadProbeReturn::Ok
    });

    debug!("Playout-delay probe installed on queue src pad (ext ID {PLAYOUT_DELAY_EXT_ID})");
}

const EXCISABLE_PREFIXES: &[&str] = &["rtpulpfecenc", "rtpredenc", "rtprtxsend"];

/// Optimise the webrtcbin send path once `connection-state → Connected`:
///
/// 1. **Excise FEC/RED/RTX encoders** – `rtpulpfecenc`, `rtpredenc`, and
///    `rtprtxsend` are created even with `fec-type=None`; we surgically
///    unlink and remove them.
/// 2. **Disable sync on all internal sinks** – setting `sync=false` on
///    every element that exposes the property (clocksync pacing elements
///    AND transport sinks like nicesink) prevents clock-based packet pacing.
/// 3. **Excise the queue** – removes the temporary queue between the tee
///    and webrtcbin so data flows directly.
///
/// All excision is done inside a `BLOCK_DOWNSTREAM` probe on the tee src
/// pad to guarantee no data races.
fn optimise_send_path(webrtcbin: &gst::Element, queue: &gst::Element) {
    let Some(tee_pad) = queue.static_pad("sink").and_then(|p| p.peer()) else {
        warn!("No tee src pad found, cannot excise queue from WebRTC send path");
        return;
    };

    let webrtcbin_weak = webrtcbin.downgrade();
    let queue_weak = queue.downgrade();
    tee_pad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_pad, _info| {
        let webrtcbin = webrtcbin_weak.upgrade();
        if let Some(bin) = webrtcbin
            .as_ref()
            .and_then(|element| element.downcast_ref::<gst::Bin>())
        {
            let elements = bin
                .iterate_recurse()
                .into_iter()
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            let mut seen = std::collections::HashSet::new();
            for element in elements {
                let name = element.name();
                if !seen.insert(name.to_string()) {
                    continue;
                }
                if EXCISABLE_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                {
                    match excise_single_element(&element) {
                        Ok(()) => debug!("Excised {name} from WebRTC send path"),
                        Err(error) => warn!("Failed to excise {name}: {error:#}"),
                    }
                }
                if force_sync_false_on_element(&element) {
                    debug!("Disabled sync on {name}");
                }
            }
        }

        if let Some(queue) = queue_weak.upgrade() {
            match excise_single_element(&queue) {
                Ok(()) => debug!("Excised queue from WebRTC send path"),
                Err(error) => error!("Failed to excise queue from WebRTC send path: {error:#}"),
            }
        }

        gst::PadProbeReturn::Remove
    });

    if crate::cli::manager::is_dot_enabled()
        && let Some(bin) = webrtcbin.downcast_ref::<gst::Bin>()
    {
        crate::stream::gst::utils::dump_bin_elements(bin, "WebRTCBin internals");
    }
}

/// Send a ForceKeyUnit event upstream so the encoder produces a fresh keyframe
/// right after the WebRTC peer connects.
fn send_force_key_unit_upstream(
    webrtcbin: &gst::Element,
    queue_weak: &glib::WeakRef<gst::Element>,
) {
    let fku_event = gst_video::UpstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
    // Request pads (sink_%u) are not returned by static_pad();
    // iterate all sink pads and pick the first one with a peer.
    let fku_pad = webrtcbin
        .iterate_sink_pads()
        .into_iter()
        .filter_map(Result::ok)
        .find_map(|pad| pad.peer())
        .or_else(|| queue_weak.upgrade().and_then(|q| q.static_pad("src")));
    let _ = fku_pad.as_ref().map(|p| p.send_event(fku_event));
    debug!("Sent ForceKeyUnit upstream on WebRTC session connect");
}

use crate::stream::gst::utils::excise_single_element;
