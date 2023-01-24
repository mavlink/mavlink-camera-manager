use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use tokio::sync::mpsc;
use tracing::*;

use gst::prelude::*;

use super::SinkInterface;
use crate::stream::manager::Manager;
use crate::stream::pipeline::PIPELINE_SINK_TEE_NAME;
use crate::stream::webrtc::signalling_protocol::{
    Answer, BindAnswer, EndSessionQuestion, IceNegotiation, MediaNegotiation, Message, Question,
    RTCIceCandidateInit, RTCSessionDescription, Sdp,
};
use crate::stream::webrtc::signalling_server::WebRTCSessionManagementInterface;
use crate::stream::webrtc::turn_server::DEFAULT_STUN_ENDPOINT;
use crate::stream::webrtc::turn_server::DEFAULT_TURN_ENDPOINT;
use crate::stream::webrtc::webrtcbin_interface::WebRTCBinInterface;

#[derive(Debug, Clone)]
pub struct WebRTCSink(Arc<Mutex<WebRTCSinkInner>>);

impl std::ops::Deref for WebRTCSink {
    type Target = Mutex<WebRTCSinkInner>;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[derive(Debug)]
pub struct WebRTCSinkInner {
    pub queue: gst::Element,
    pub webrtcbin: gst::Element,
    pub webrtcbin_sink_pad: gst::Pad,
    pub tee_src_pad: Option<gst::Pad>,
    pub bind: BindAnswer,
    /// MPSC channel's sender to send messages to the respective Websocket from Signaller server. Err can be used to end the WebSocket.
    pub sender: Mutex<mpsc::UnboundedSender<Result<Message>>>,
    pub end_reason: Option<String>,
}
impl SinkInterface for WebRTCSink {
    #[instrument(level = "debug", skip(self))]
    fn link(
        self: &mut WebRTCSink,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        let mut inner = self.0.lock().unwrap();

        // Configure transceiver
        let webrtcbin_sink_pad = &inner.webrtcbin_sink_pad;
        let transceiver =
            webrtcbin_sink_pad.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
        transceiver.set_property(
            "direction",
            gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
        );

        // Link
        inner.link(pipeline, pipeline_id, tee_src_pad)?;

        // TODO: Workaround for bug: https://gitlab.freedesktop.org/gstreamer/gst-plugins-bad/-/issues/1539
        // Reasoning: because we are not receiving the Disconnected | Failed | Closed of WebRTCPeerConnectionState,
        // we are directly connecting to webrtcbin->transceiver->transport->connect_state_notify:
        // When the bug is solved, we should remove this code and use WebRTCPeerConnectionState instead.
        let weak_this = self.clone();
        let rtp_sender = transceiver.sender().unwrap();
        rtp_sender.connect_notify(Some("transport"), move |rtp_sender, _pspec| {
            let transport = rtp_sender.property::<gst_webrtc::WebRTCDTLSTransport>("transport");

            debug!("DTLS Transport: {:#?}", transport);

            let weak_this = weak_this.clone();
            transport.connect_state_notify(move |transport| {
                use gst_webrtc::WebRTCDTLSTransportState::*;

                let state = transport.state();
                debug!("DTLS Transport Connection changed to {state:#?}");
                match state {
                    Failed | Closed => {
                        if weak_this.lock().unwrap().webrtcbin.current_state()
                            == gst::State::Playing
                        {
                            weak_this.close("DTLS Transport connection lost")
                        }
                    }
                    _ => (),
                }
            });
        });

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let inner = self.0.lock().unwrap(); // We are getting locked here!
        inner.unlink(pipeline, pipeline_id)
    }

    #[instrument(level = "trace", skip(self))]
    fn get_id(&self) -> uuid::Uuid {
        self.0.lock().unwrap().get_id()
    }

    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        self.0.lock().unwrap().get_sdp()
    }
}

impl WebRTCSink {
    #[instrument(level = "debug")]
    pub fn try_new(
        bind: BindAnswer,
        sender: mpsc::UnboundedSender<Result<Message>>,
    ) -> Result<Self> {
        sender.send(Ok(Message::from(Answer::StartSession(bind.clone()))))?;

        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .property_from_str("name", format!("webrtcbin-{}", bind.session_id).as_str())
            .property("async-handling", true)
            .property("bundle-policy", gst_webrtc::WebRTCBundlePolicy::MaxBundle) // https://webrtcstandards.info/sdp-bundle/
            .property("latency", 0u32)
            .property_from_str("stun-server", DEFAULT_STUN_ENDPOINT)
            .property_from_str("turn-server", DEFAULT_TURN_ENDPOINT)
            .build()?;

        let webrtcbin_sink_pad = webrtcbin
            .request_pad_simple("sink_%u")
            .context("Failed requesting sink pad for webrtcsink")?;

        let this = WebRTCSink(Arc::new(Mutex::new(WebRTCSinkInner {
            queue,
            webrtcbin,
            webrtcbin_sink_pad,
            tee_src_pad: None,
            bind,
            sender: Mutex::new(sender),
            end_reason: None,
        })));

        let mut this_guard_result = this.lock();
        let this_guard = this_guard_result.as_deref_mut().unwrap();
        let webrtcbin = &this_guard.webrtcbin;

        // Connect to on-negotiation-needed to handle sending an Offer
        let weak_this = this.clone();
        webrtcbin.connect("on-negotiation-needed", false, move |values| {
            let element = values[0].get::<gst::Element>().expect("Invalid argument");

            if let Err(error) = weak_this.on_negotiation_needed(&element) {
                error!("Failed to negotiate: {error:?}");
            }

            None
        });

        // Whenever there is a new ICE candidate, send it to the peer
        let weak_this = this.clone();
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            let element = values[0].get::<gst::Element>().expect("Invalid argument");
            let sdp_m_line_index = values[1].get::<u32>().expect("Invalid argument");
            let candidate = values[2].get::<String>().expect("Invalid argument");

            if let Err(error) = weak_this.on_ice_candidate(&element, &sdp_m_line_index, &candidate)
            {
                debug!("Failed to send ICE candidate: {error:#?}");
            }
            None
        });

        let weak_this = this.clone();
        webrtcbin.connect_notify(Some("connection-state"), move |webrtcbin, _pspec| {
            let state =
                webrtcbin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");

            weak_this.on_connection_state_change(webrtcbin, &state);
        });

        let weak_this = this.clone();
        webrtcbin.connect_notify(Some("ice-connection-state"), move |webrtcbin, _pspec| {
            let state =
                webrtcbin.property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");

            weak_this.on_ice_connection_state_change(webrtcbin, &state);
        });

        let weak_this = this.clone();
        webrtcbin.connect_notify(Some("ice-gathering-state"), move |webrtcbin, _pspec| {
            let state =
                webrtcbin.property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state");

            weak_this.on_ice_gathering_state_change(webrtcbin, &state);
        });

        drop(this_guard_result);
        Ok(this)
    }

    #[instrument(level = "trace", skip(self))]
    pub fn send(&self, message: Message) -> Result<()> {
        self.lock()
            .unwrap()
            .sender
            .lock()
            .unwrap()
            .send(Ok(message))
            .context("Failed sending message {message:#?}")
    }

    #[instrument(level = "debug", skip(self))]
    pub fn close(&self, reason: &str) {
        let bind = &self.lock().unwrap().bind.clone();
        if let Err(error) = Manager::remove_session(bind, reason.to_string()) {
            error!("Failed removing session {bind:#?}. Reason: {error}");
        }
    }
}

impl WebRTCBinInterface for WebRTCSink {
    // Whenever webrtcbin tells us that (re-)negotiation is needed, simply ask
    // for a new offer SDP from webrtcbin without any customization and then
    // asynchronously send it to the peer via the WebSocket connection
    // #[instrument(level = "debug", skip(self))]
    fn on_negotiation_needed(&self, webrtcbin: &gst::Element) -> Result<()> {
        let this = self.clone();
        let webrtcbin_clone = webrtcbin.clone();
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

            debug!(
                "Sending SDP offer to peer. Offer: {}",
                offer.sdp().as_text().unwrap()
            );

            if let Err(error) = this.on_offer_created(&webrtcbin_clone, &offer) {
                error!("Failed to send SDP offer: {error:?}");
            }
        });

        webrtcbin.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);

        Ok(())
    }

    // Once webrtcbin has create the offer SDP for us, handle it by sending it to the peer via the
    // WebSocket connection
    #[instrument(level = "debug", skip(self))]
    fn on_offer_created(
        &self,
        _webrtcbin: &gst::Element,
        offer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        self.lock()
            .unwrap()
            .webrtcbin
            .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

        // Here we hack the SDP lying about our the profile-level-id (to constrained-baseline) so any browser can accept it
        let sdp = offer.sdp().as_text().unwrap();
        let sdp = regex::Regex::new("level-asymmetry-allowed=[01]")
            .unwrap()
            .replace(&sdp, "");
        let sdp = regex::Regex::new(";;").unwrap().replace(&sdp, ";");
        let sdp = regex::Regex::new("profile-level-id=[[:xdigit:]]{6}")
            .unwrap()
            .replace(&sdp, "profile-level-id=42e01f;level-asymmetry-allowed=1")
            .to_string();

        let message = MediaNegotiation {
            bind: self.lock().unwrap().bind.clone(),
            sdp: RTCSessionDescription::Offer(Sdp { sdp }),
        }
        .into();

        self.send(message).context("Failed to send SDP offer")?;

        debug!("SDP offer created!");

        Ok(())
    }

    // Once webrtcbin has create the answer SDP for us, handle it by sending it to the peer via the
    // WebSocket connection
    #[instrument(level = "debug", skip(self))]
    fn on_answer_created(
        &self,
        _webrtcbin: &gst::Element,
        answer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        self.lock()
            .unwrap()
            .webrtcbin
            .emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);

        debug!(
            "sending SDP answer to peer: {}",
            answer.sdp().as_text().unwrap()
        );

        let message = MediaNegotiation {
            bind: self.lock().unwrap().bind.clone(),
            sdp: RTCSessionDescription::Answer(Sdp {
                sdp: answer.sdp().as_text().unwrap(),
            }),
        }
        .into();

        self.send(message).context("Failed to send SDP answer")?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn on_ice_candidate(
        &self,
        _webrtcbin: &gst::Element,
        sdp_m_line_index: &u32,
        candidate: &str,
    ) -> Result<()> {
        let message = IceNegotiation {
            bind: self.lock().unwrap().bind.clone(),
            ice: RTCIceCandidateInit {
                candidate: Some(candidate.to_owned()),
                sdp_mid: None,
                sdp_m_line_index: Some(sdp_m_line_index.to_owned()),
                username_fragment: None,
            },
        }
        .into();

        self.send(message).context("Failed to send ICE candidate")?;

        debug!("ICE candidate created!");

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn on_ice_gathering_state_change(
        &self,
        _webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEGatheringState,
    ) {
        if let gst_webrtc::WebRTCICEGatheringState::Complete = state {
            debug!("ICE gathering complete")
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn on_ice_connection_state_change(
        &self,
        _webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEConnectionState,
    ) {
        use gst_webrtc::WebRTCICEConnectionState::*;

        debug!("ICE connection changed to {state:#?}");
        match state {
            Failed | Closed | Disconnected => self.close("ICE closed"),
            _ => (),
        };
    }

    #[instrument(level = "debug", skip(self))]
    fn on_connection_state_change(
        &self,
        webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCPeerConnectionState,
    ) {
        use gst_webrtc::WebRTCPeerConnectionState::*;

        debug!("Connection changed to {state:#?}");
        match state {
            // TODO: This would be the desired workflow, but it is not being detected, so we are using a workaround connecting direcly to the DTLS Transport connection state in the Session constructor.
            Disconnected | Failed | Closed => {
                warn!("For mantainers: Peer connection lost was detected by WebRTCPeerConnectionState, we should remove the workaround. State: {state:#?}");
                // self.close("Connection lost"); // TODO: Keep this line commented until the forementioned bug is solved.
            }
            _ => (),
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn handle_sdp(&self, sdp: &gst_webrtc::WebRTCSessionDescription) -> Result<()> {
        self.lock()
            .unwrap()
            .webrtcbin
            .emit_by_name::<()>("set-remote-description", &[&sdp, &None::<gst::Promise>]);
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn handle_ice(&self, sdp_m_line_index: &u32, candidate: &str) -> Result<()> {
        self.lock()
            .unwrap()
            .webrtcbin
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
        Ok(())
    }
}

impl SinkInterface for WebRTCSinkInner {
    #[instrument(level = "debug", skip(self))]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
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

        // Add the Sink elements to the Pipeline
        let elements = &[&self.queue, &self.webrtcbin];
        if let Err(error) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed to add WebRTCBin {sink_id} to Pipeline {pipeline_id}. Reason: {error:#?}"
            ));
        }

        // Link the queue's src pad to the Sink's sink pad
        let queue_src_pad = &self
            .queue
            .static_pad("src")
            .expect("No sink pad found on Queue");
        if let Err(error) = queue_src_pad.link(&self.webrtcbin_sink_pad) {
            pipeline.remove_many(elements)?;
            return Err(anyhow!(error));
        }

        // Link the new Tee's src pad to the queue's sink pad
        let queue_sink_pad = &self
            .queue
            .static_pad("sink")
            .expect("No src pad found on Queue");
        if let Err(error) = tee_src_pad.link(queue_sink_pad) {
            pipeline.remove_many(elements)?;
            queue_src_pad.unlink(&self.webrtcbin_sink_pad)?;
            return Err(anyhow!(error));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let sink_id = self.get_id();

        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink sink {sink_id} from pipeline {pipeline_id} without a Tee src pad.");
            return Ok(());
        };

        let queue_sink_pad = self
            .queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(error) = tee_src_pad.unlink(&queue_sink_pad) {
            return Err(anyhow!(
                "Failed unlinking Sink {sink_id} from Tee's source pad. Reason: {error:?}"
            ));
        }
        drop(queue_sink_pad);

        let elements = &[&self.queue, &self.webrtcbin];
        if let Err(error) = pipeline.remove_many(elements) {
            return Err(anyhow!(
                "Failed removing WebRTCBin element {sink_id} from pipeline {pipeline_id}. Reason: {error:?}"
            ));
        }

        if let Err(error) = self.queue.set_state(gst::State::Null) {
            return Err(anyhow!(
                "Failed to set queue from sink {sink_id} state to NULL. Reason: {error:#?}"
            ));
        }

        let sink_name = format!("{PIPELINE_SINK_TEE_NAME}-{pipeline_id}");
        let tee = pipeline
            .by_name(&sink_name)
            .context(format!("no element named {sink_name:#?}"))?;
        if let Err(error) = tee.remove_pad(tee_src_pad) {
            return Err(anyhow!(
                "Failed removing Tee's source pad. Reason: {error:?}"
            ));
        }

        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    fn get_id(&self) -> uuid::Uuid {
        self.bind.session_id
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available. Reason: WebRTC Sink should only be connected by means of its Signalling protocol."
        ))
    }
}

impl Drop for WebRTCSinkInner {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        // Send EndSession to consumers, if the WebSocket and MPSC are still available
        if let Ok(sender) = self.sender.lock() {
            let question = Question::EndSession(EndSessionQuestion {
                bind: self.bind.clone(),
                reason: self
                    .end_reason
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            });

            if let Err(reason) = sender.send(Ok(Message::from(question))) {
                error!("Failed to send EndSession question to MPSC channel. Reason: {reason}");
            }
        }
    }
}
