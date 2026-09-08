use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tokio::sync::mpsc::{self, WeakUnboundedSender};
use tracing::*;

use crate::{
    cli,
    stream::{
        gst::utils::{excise_single_element, wait_for_element_state},
        pipeline::runner::PipelineRunner,
        webrtc::{
            signalling_protocol::{
                Answer, BindAnswer, EndSessionQuestion, IceNegotiation, MediaNegotiation, Message,
                Question, RTCIceCandidateInit, RTCSessionDescription, Sdp,
            },
            webrtcbin_interface::WebRTCBinInterface,
        },
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    SinkInterface, force_sync_false_on_element, link_sink_to_tee, make_proxy_bridge,
    unlink_sink_from_tee,
};

const PLAYOUT_DELAY_URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";
const PLAYOUT_DELAY_EXT_ID: u8 = 13;

#[derive(Clone)]
pub struct WebRTCSinkWeakProxy {
    bind: BindAnswer,
    sender: WeakUnboundedSender<Result<Message>>,
    stats: Arc<Mutex<SessionStats>>,
}

#[derive(Debug)]
pub struct WebRTCSink {
    sink_id: Arc<uuid::Uuid>,
    pipeline: gst::Pipeline,
    proxysink: gst::Element,
    proxysrc: gst::Element,
    webrtcbin: gst::Element,
    webrtcbin_sink_pad: Option<gst::Pad>,
    tee_src_pad: Option<gst::Pad>,
    bind: BindAnswer,
    /// MPSC channel's sender to send messages to the respective Websocket from Signaller server. Err can be used to end the WebSocket.
    sender: mpsc::UnboundedSender<Result<Message>>,
    block_probe_id: Arc<Mutex<Option<gst::PadProbeId>>>,
    block_pad: Arc<Mutex<Option<glib::WeakRef<gst::Pad>>>>,
    pipeline_runner: PipelineRunner,
    /// Per-session observability counters updated by the various WebRTC
    /// callbacks. Summarised in `Drop` for a single end-of-session log
    /// line and consulted by the FailSafeKiller to explain timeouts.
    stats: Arc<Mutex<SessionStats>>,
    end_reason: Option<String>,
}

impl Drop for WebRTCSink {
    fn drop(&mut self) {
        if let Ok(stats) = self.stats.lock() {
            let duration_ms = stats.started_at.elapsed().as_millis() as u64;
            let ms_since = |instant: Option<std::time::Instant>| -> Option<u128> {
                instant.map(|t| t.duration_since(stats.started_at).as_millis())
            };
            info!(
                session_id = %self.bind.session_id,
                producer_id = %self.bind.producer_id,
                consumer_id = %self.bind.consumer_id,
                duration_ms,
                peer_state = ?stats.peer_connection_state,
                ice_state = ?stats.ice_connection_state,
                gather_state = ?stats.ice_gathering_state,
                upstream_encoding = stats.upstream_encoding.as_deref().unwrap_or("?"),
                local_ice_total = stats.local_ice.total(),
                local_ice_host = stats.local_ice.host,
                local_ice_srflx = stats.local_ice.srflx,
                local_ice_relay = stats.local_ice.relay,
                remote_ice_total = stats.remote_ice.total(),
                remote_ice_host = stats.remote_ice.host,
                remote_ice_srflx = stats.remote_ice.srflx,
                remote_ice_relay = stats.remote_ice.relay,
                ms_to_offer = ?ms_since(stats.offer_sent_at),
                ms_to_answer = ?ms_since(stats.answer_received_at),
                ms_to_first_client_ice = ?ms_since(stats.first_client_ice_at),
                ms_to_connected = ?ms_since(stats.connected_at),
                ms_to_first_frame = ?ms_since(stats.first_frame_at),
                reason = %stats.end_reason.as_deref()
                    .or(self.end_reason.as_deref())
                    .unwrap_or("dropped"),
                "WebRTC session ended",
            );
        }
        if let Some(pad) = self.webrtcbin_sink_pad.take() {
            self.webrtcbin.release_request_pad(&pad);
        }
        let _ = self.webrtcbin.set_state(gst::State::Null);
        if let Err(error) =
            wait_for_element_state(self.webrtcbin.downgrade(), gst::State::Null, 100, 5)
        {
            warn!("webrtcbin did not reach Null within 5 s on drop: {error:?}");
        }

        // `READY_TO_NULL` only quits the WebRTCBin GMainLoop; the joining
        // `g_thread_join` runs later, from `gst_webrtc_bin_finalize`, when
        // the GObject refcount reaches zero. The session sub-pipeline still
        // holds a child-strong ref on webrtcbin here, so without explicitly
        // detaching, the finalize never fires and we leak exactly one
        // "WebRTCBin" thread per session. Drop the parent ref synchronously
        // so that when `self.webrtcbin` is dropped on field-drop, refcount
        // hits zero and the thread is joined.
        if let Err(error) = self.pipeline.remove(&self.webrtcbin) {
            warn!("Failed removing webrtcbin from session sub-pipeline on drop: {error:?}");
        }

        let _ = self.pipeline.set_state(gst::State::Null);
        if let Err(error) =
            wait_for_element_state(self.pipeline.downgrade(), gst::State::Null, 100, 5)
        {
            warn!("session sub-pipeline did not reach Null within 5 s on drop: {error:?}");
        }
    }
}
impl SinkInterface for WebRTCSink {
    #[instrument(level = "debug", skip(self, pipeline), fields(session_id = %self.bind.session_id, producer_id = %self.bind.producer_id))]
    fn link(
        self: &mut WebRTCSink,
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from Sink {:?} has already been configured",
                self.get_id()
            ));
        }

        // Configure transceiver https://gstreamer.freedesktop.org/documentation/webrtclib/gstwebrtc-transceiver.html?gi-language=c
        let webrtcbin_sink_pad = self
            .webrtcbin_sink_pad
            .as_ref()
            .context("webrtcbin_sink_pad already consumed")?;
        let transceiver =
            webrtcbin_sink_pad.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");
        let first_frame_stats = Arc::clone(&self.stats);
        let first_frame_session_id = self.bind.session_id;
        webrtcbin_sink_pad.add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |_pad, _info| {
                if let Ok(mut stats) = first_frame_stats.lock() {
                    if stats.first_frame_at.is_none() {
                        stats.first_frame_at = Some(std::time::Instant::now());
                        let ms_since_start = stats.started_at.elapsed().as_millis() as u64;
                        debug!(
                            session_id = %first_frame_session_id,
                            ms_since_start,
                            "First RTP buffer reached webrtcbin sink pad",
                        );
                    }
                }
                gst::PadProbeReturn::Ok
            },
        );
        transceiver.set_property(
            "direction",
            gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly,
        );
        transceiver.set_property("do-nack", true); // Enable retransmission (RFC4588)
        transceiver.set_property("fec-type", gst_webrtc::WebRTCFECType::None);

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

        let elements = &[&self.proxysink];
        link_sink_to_tee(tee_src_pad, pipeline, elements)?;

        // Provide codec-preferences so webrtcbin can create an SDP offer
        // without waiting for buffer caps on the sink pad. `add_sink()` waits
        // for fixed upstream RTP caps before calling this sync path.
        if let Some(caps) = tee_src_pad
            .parent_element()
            .and_then(|tee| tee.static_pad("sink"))
            .and_then(|tee_sink| {
                tee_sink
                    .current_caps()
                    .or_else(|| tee_sink.peer().and_then(|peer| peer.current_caps()))
            })
        {
            if let Some(structure) = caps.structure(0) {
                let encoding = structure.get::<&str>("encoding-name").unwrap_or("");
                let mut codec_caps = gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("encoding-name", encoding)
                    .field(
                        "clock-rate",
                        structure.get::<i32>("clock-rate").unwrap_or(90000),
                    )
                    .field("payload", structure.get::<i32>("payload").unwrap_or(96))
                    .build();

                if let Some(codec_structure) = codec_caps.make_mut().structure_mut(0) {
                    match encoding {
                        "H264" => {
                            for key in [
                                "profile-level-id",
                                "packetization-mode",
                                "sprop-parameter-sets",
                            ] {
                                if let Ok(value) = structure.get::<&str>(key) {
                                    codec_structure.set(key, value);
                                }
                            }
                        }
                        "H265" => {
                            for key in [
                                "profile-id",
                                "profile-space",
                                "tier-flag",
                                "level-id",
                                "sprop-vps",
                                "sprop-sps",
                                "sprop-pps",
                            ] {
                                if let Ok(value) = structure.get::<&str>(key) {
                                    codec_structure.set(key, value);
                                }
                            }
                        }
                        _ => (),
                    }
                }

                if let Ok(mut stats) = self.stats.lock()
                    && !encoding.is_empty()
                {
                    stats.upstream_encoding = Some(encoding.to_string());
                }
                debug!(
                    session_id = %self.bind.session_id,
                    "Setting codec-preferences: {codec_caps}",
                );
                transceiver.set_property("codec-preferences", &codec_caps);
            }
        } else {
            warn!(
                session_id = %self.bind.session_id,
                "No profile caps available upstream of tee for codec-preferences",
            );
        }

        // Block data at the boundary between the proxy bridge and webrtcbin
        // so no buffers reach the internal RED/FEC/RTX encoders before they
        // are excised on Connected. On "warm" connections the tee already
        // has data flowing; without this block those buffers would pass
        // through rtpredenc (payload type not in the SDP), causing permanent
        // codec resolution failure in the browser. The probe is removed
        // explicitly from `optimise_send_path` once excision finishes.
        //
        // Prefer the proxysrc's internal queue src pad over the ghost src
        // pad: with the queue in place, blocking the queue's src holds
        // buffers in the queue (sized by a 5 s time cap, see
        // `configure_proxysrc_queue`) and never reaches into `proxysink`
        // to backpressure the producer tee.
        let block_pad = self
            .proxysrc
            .downcast_ref::<gst::Bin>()
            .and_then(|bin| {
                bin.children()
                    .into_iter()
                    .find(|element| element.name().starts_with("queue"))
            })
            .and_then(|q| q.static_pad("src"))
            .or_else(|| self.proxysrc.static_pad("src"));
        if let Some(pad) = block_pad {
            if let Some(probe_id) = pad.add_probe(
                gst::PadProbeType::BLOCK
                    | gst::PadProbeType::BUFFER
                    | gst::PadProbeType::BUFFER_LIST,
                |_pad, _info| gst::PadProbeReturn::Ok,
            ) {
                *self.block_probe_id.lock().expect("block_probe_id mutex") = Some(probe_id);
                *self.block_pad.lock().expect("block_pad mutex") = Some(pad.downgrade());
                debug!("Installed pre-excision block on proxysrc queue src pad");
            } else {
                warn!("Failed to install pre-excision block on proxysrc queue src pad");
            }
        } else {
            warn!("Could not find a pad to install the pre-excision block on");
        }

        // The playout-delay probe is installed on the proxysrc ghost src pad
        // (the boundary into webrtcbin). That pad outlives any individual
        // child and carries every outgoing RTP buffer destined for the peer.
        if let Some(pad) = self.proxysrc.static_pad("src") {
            install_playout_delay_probe(&pad);
        } else {
            warn!("install_playout_delay_probe: proxysrc has no src pad");
        }

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

        let elements = &[&self.proxysink];
        unlink_sink_from_tee(tee_src_pad, pipeline, elements)?;

        Ok(())
    }

    #[instrument(level = "trace", skip(self))]
    fn get_id(&self) -> Arc<uuid::Uuid> {
        self.sink_id.clone()
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "WebRTC Sink can only be connected via its Signalling protocol"
        ))
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        self.pipeline_runner.start()
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
        Some(&self.pipeline)
    }
}

impl WebRTCSink {
    #[instrument(level = "debug", skip(sender, video_and_stream_information))]
    pub fn try_new(
        bind: BindAnswer,
        sender: mpsc::UnboundedSender<Result<Message>>,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self> {
        let [proxysink, proxysrc] = make_proxy_bridge()?;

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

        let webrtcbin_sink_pad = webrtcbin
            .request_pad_simple("sink_%u")
            .context("Failed to get Sink Pad")?;

        // Create the pipeline
        let sink_id = Arc::new(bind.session_id);
        let pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-webrtc-sink-{sink_id}"))
            .build();

        // Add Sink elements to the Sink's Pipeline
        let elements = [&proxysrc, &webrtcbin];
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding WebRTCSink's elements to Sink Pipeline: {add_err:?}"
            ));
        }

        // Link Sink's elements
        if let Err(link_err) = gst::Element::link_many(elements) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from WebRTCSink Pipeline: {remove_err:?}")
            };
            return Err(anyhow!(
                "Failed linking WebRTCSink's elements: {link_err:?}"
            ));
        }

        let pipeline_runner =
            PipelineRunner::try_new(&pipeline, &sink_id, true, video_and_stream_information)?;

        sender.send(Ok(Message::from(Answer::StartSession(bind.clone()))))?;

        let stun = cli::manager::stun_server_address();
        let turn_count = cli::manager::turn_server_addresses().len();
        let stun_resolvable = resolve_webrtc_endpoint(&stun).is_ok();
        info!(
            session_id = %bind.session_id,
            stun_url = %stun,
            stun_resolvable,
            turn_count,
            "WebRTC session starting",
        );

        let this = WebRTCSink {
            sink_id,
            pipeline,
            proxysink,
            proxysrc,
            webrtcbin,
            webrtcbin_sink_pad: Some(webrtcbin_sink_pad),
            tee_src_pad: None,
            bind,
            sender,
            block_probe_id: Arc::new(Mutex::new(None)),
            block_pad: Arc::new(Mutex::new(None)),
            pipeline_runner,
            stats: Arc::new(Mutex::new(SessionStats::new())),
            end_reason: None,
        };

        let (peer_connected_tx, peer_connected_rx) = std::sync::mpsc::channel::<PeerEvent>();

        // Fallback killer: if neither the `connection-state` notify nor
        // the ICE state callbacks reach a terminal state, this thread
        // ends the session after the grace period. The previous 10-second
        // timer was too aggressive -- on slower links DTLS/ICE routinely
        // takes more than 10 s to reach Connected, racing the killer
        // with the negotiator. The killer now also exits early on an
        // explicit `PeerEvent::Failed` from the ICE / connection state
        // callbacks, so we don't wait the full grace period for obvious
        // failures.
        const NEGOTIATION_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
        let weak_proxy = this.downgrade();
        std::thread::Builder::new()
            .name("FailSafeKiller".to_string())
            .spawn(move || {
                debug!(
                    session_id = %weak_proxy.bind.session_id,
                    "Waiting for peer to be connected within {}s...",
                    NEGOTIATION_GRACE.as_secs(),
                );

                match peer_connected_rx.recv_timeout(NEGOTIATION_GRACE) {
                    Ok(PeerEvent::Connected) => {
                        debug!(
                            session_id = %weak_proxy.bind.session_id,
                            "Peer connected. Disabling FailSafeKiller",
                        );
                        return;
                    }
                    Ok(PeerEvent::Failed) => {
                        warn!(
                            session_id = %weak_proxy.bind.session_id,
                            "ICE/DTLS reported terminal failure. FailSafeKiller ending session early.",
                        );
                        if let Err(error) =
                            weak_proxy.terminate("ICE/DTLS failure".to_string())
                        {
                            error!("Failed sending EndSessionQuestion: {error}");
                        }
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        debug!(
                            session_id = %weak_proxy.bind.session_id,
                            "Session ended before negotiation timeout. FailSafeKiller exiting.",
                        );
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }

                let cause = weak_proxy
                    .stats
                    .lock()
                    .map(|s| s.likely_cause_at_negotiation_timeout())
                    .unwrap_or("stats lock poisoned");

                {
                    let bind = &weak_proxy.bind;
                    if let Ok(stats) = weak_proxy.stats.lock() {
                        warn!(
                            session_id = %bind.session_id,
                            producer_id = %bind.producer_id,
                            grace_secs = NEGOTIATION_GRACE.as_secs(),
                            peer_state = ?stats.peer_connection_state,
                            ice_state = ?stats.ice_connection_state,
                            gather_state = ?stats.ice_gathering_state,
                            local_ice_total = stats.local_ice.total(),
                            local_ice_host = stats.local_ice.host,
                            local_ice_srflx = stats.local_ice.srflx,
                            local_ice_relay = stats.local_ice.relay,
                            remote_ice_total = stats.remote_ice.total(),
                            offer_sent = stats.offer_sent_at.is_some(),
                            answer_received = stats.answer_received_at.is_some(),
                            likely_cause = cause,
                            "FailSafeKiller: WebRTC negotiation timed out, killing session",
                        );
                    } else {
                        warn!(
                            session_id = %bind.session_id,
                            grace_secs = NEGOTIATION_GRACE.as_secs(),
                            "FailSafeKiller: WebRTC negotiation timed out, killing session (stats unavailable)",
                        );
                    }
                }

                if let Err(error) = weak_proxy.terminate(format!(
                    "WebRTC negotiation timeout: {cause}"
                )) {
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
        let block_pad_slot = this.block_pad.clone();
        let block_probe_slot = this.block_probe_id.clone();
        this.webrtcbin
            .connect_notify(Some("connection-state"), move |webrtcbin, _pspec| {
                let state =
                    webrtcbin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");

                if matches!(state, gst_webrtc::WebRTCPeerConnectionState::Connected) {
                    if let Err(error) = peer_connected_tx.send(PeerEvent::Connected) {
                        error!("Failed to disable FailSafeKiller: {error:?}");
                    }

                    optimise_send_path(webrtcbin, &block_pad_slot, &block_probe_slot);

                    send_force_key_unit_upstream(webrtcbin);
                }

                if matches!(
                    state,
                    gst_webrtc::WebRTCPeerConnectionState::Failed
                        | gst_webrtc::WebRTCPeerConnectionState::Closed
                        | gst_webrtc::WebRTCPeerConnectionState::Disconnected
                ) {
                    if let Err(error) = peer_connected_tx.send(PeerEvent::Failed) {
                        debug!("Failed to notify FailSafeKiller of ICE failure: {error:?}");
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
            stats: Arc::clone(&self.stats),
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

    pub fn bind(&self) -> &BindAnswer {
        &self.bind
    }
}

impl WebRTCSinkWeakProxy {
    fn terminate(&self, reason: String) -> Result<()> {
        let Some(sender) = self.sender.upgrade() else {
            return Err(anyhow!("Failed accessing MPSC Sender"));
        };

        if let Ok(mut stats) = self.stats.lock() {
            if stats.end_reason.is_none() {
                stats.end_reason = Some(reason.clone());
            }
        }

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
    #[instrument(level = "debug", skip_all, fields(session_id = %self.bind.session_id))]
    fn on_offer_created(
        &self,
        webrtcbin: &gst::Element,
        offer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        // Recreate the SDP offer with our customized SDP
        let offer = gst_webrtc::WebRTCSessionDescription::new(
            offer.type_(),
            customize_sent_sdp(offer.sdp(), Some((&self.bind, &self.stats)))?,
        );

        let Ok(sdp) = offer.sdp().as_text() else {
            return Err(anyhow!("Failed reading the created SDP"));
        };

        // All good, then set local description
        webrtcbin.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

        if let Ok(mut stats) = self.stats.lock() {
            stats
                .offer_sent_at
                .get_or_insert_with(std::time::Instant::now);
        }

        debug!(
            session_id = %self.bind.session_id,
            "Sending SDP offer to peer. Offer:\n{sdp}",
        );

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
    #[instrument(level = "debug", skip(self, _webrtcbin), fields(session_id = %self.bind.session_id))]
    fn on_answer_created(
        &self,
        _webrtcbin: &gst::Element,
        answer: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        // Recreate the SDP answer with our customized SDP
        let answer = gst_webrtc::WebRTCSessionDescription::new(
            answer.type_(),
            customize_sent_sdp(answer.sdp(), Some((&self.bind, &self.stats)))?,
        );

        let Ok(sdp) = answer.sdp().as_text() else {
            return Err(anyhow!("Failed reading the answer SDP"));
        };

        debug!(
            session_id = %self.bind.session_id,
            "Sending SDP answer to peer. Answer:\n{sdp}",
        );

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

    #[instrument(level = "debug", skip(self, _webrtcbin), fields(session_id = %self.bind.session_id))]
    fn on_ice_candidate(
        &self,
        _webrtcbin: &gst::Element,
        sdp_m_line_index: &u32,
        candidate: &str,
    ) -> Result<()> {
        let kind = classify_ice_candidate(candidate);
        if let Ok(mut stats) = self.stats.lock() {
            stats.local_ice.increment(kind);
        }

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

        debug!(
            session_id = %self.bind.session_id,
            kind = kind.as_str(),
            "Local ICE candidate created: {candidate}",
        );

        Ok(())
    }

    #[instrument(level = "debug", skip(self, _webrtcbin), fields(session_id = %self.bind.session_id))]
    fn on_ice_gathering_state_change(
        &self,
        _webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEGatheringState,
    ) -> Result<()> {
        if let Ok(mut stats) = self.stats.lock() {
            stats.ice_gathering_state = Some(*state);
        }
        if let gst_webrtc::WebRTCICEGatheringState::Complete = state {
            debug!(
                session_id = %self.bind.session_id,
                "ICE gathering complete",
            )
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self, webrtcbin), fields(session_id = %self.bind.session_id))]
    fn on_ice_connection_state_change(
        &self,
        webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCICEConnectionState,
    ) -> Result<()> {
        use gst_webrtc::WebRTCICEConnectionState::*;

        if let Ok(mut stats) = self.stats.lock() {
            stats.ice_connection_state = Some(*state);
        }
        debug!(
            session_id = %self.bind.session_id,
            "ICE connection changed to {state:#?}",
        );
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

    #[instrument(level = "debug", skip(self, _webrtcbin), fields(session_id = %self.bind.session_id))]
    fn on_connection_state_change(
        &self,
        _webrtcbin: &gst::Element,
        state: &gst_webrtc::WebRTCPeerConnectionState,
    ) -> Result<()> {
        use gst_webrtc::WebRTCPeerConnectionState::*;

        if let Ok(mut stats) = self.stats.lock() {
            stats.peer_connection_state = Some(*state);
            if matches!(state, Connected) {
                stats
                    .connected_at
                    .get_or_insert_with(std::time::Instant::now);
            }
        }

        debug!(
            session_id = %self.bind.session_id,
            "Connection changed to {state:#?}",
        );
        match state {
            // TODO: This would be the desired workflow, but it is not being detected, so we are using a workaround connecting direcly to the DTLS Transport connection state in the Session constructor.
            Disconnected | Failed | Closed => {
                self.terminate(format!("Connectiong closed with: {state:?}"))?;
            }
            _ => (),
        }

        Ok(())
    }

    #[instrument(level = "debug", skip_all, fields(session_id = %self.bind.session_id))]
    fn handle_sdp(
        &self,
        webrtcbin: &gst::Element,
        sdp: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<()> {
        let remote_sdp = webrtcbin
            .property::<Option<gst_webrtc::WebRTCSessionDescription>>("remote-description");

        if matches!(sdp.type_(), gst_webrtc::WebRTCSDPType::Answer) {
            if let Ok(mut stats) = self.stats.lock() {
                stats
                    .answer_received_at
                    .get_or_insert_with(std::time::Instant::now);
            }
        }

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

        let sdp = gst_webrtc::WebRTCSessionDescription::new(sdp.type_(), sanitize_sdp(sdp)?);

        if let Ok(sdp_str) = sdp.sdp().as_text() {
            debug!(
                "Received SDP (Sanitized) (type: {}):\n{sdp_str}",
                sdp.type_()
            );
        };

        // `set-remote-description` dispatches into webrtcbin / libnice / the
        // SRTP negotiator. Pathological SDPs have historically aborted the
        // whole process from inside this call with no Rust-side trace. Guard
        // against any Rust panic that bubbles up through glib so we get a
        // clean error instead of a silent process death. A native SIGSEGV
        // is still covered by `helper::diagnostics::install_crash_handlers`.
        let webrtcbin_for_panic = webrtcbin.clone();
        let sdp_for_panic = sdp.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            webrtcbin_for_panic.emit_by_name::<()>(
                "set-remote-description",
                &[&sdp_for_panic, &None::<gst::Promise>],
            );
        }));
        if let Err(panic) = result {
            let message = panic_message(&panic);
            error!("Panic while calling set-remote-description: {message}");
            return Err(anyhow!(
                "Panic while applying remote SDP description: {message}"
            ));
        }
        Ok(())
    }

    #[instrument(level = "debug", skip(self, webrtcbin), fields(session_id = %self.bind.session_id))]
    fn handle_ice(
        &self,
        webrtcbin: &gst::Element,
        sdp_m_line_index: &u32,
        candidate: &str,
    ) -> Result<()> {
        let kind = classify_ice_candidate(candidate);
        if let Ok(mut stats) = self.stats.lock() {
            stats
                .first_client_ice_at
                .get_or_insert_with(std::time::Instant::now);
            stats.remote_ice.increment(kind);
        }

        debug!(
            session_id = %self.bind.session_id,
            kind = kind.as_str(),
            "Remote ICE candidate received: {candidate}",
        );

        webrtcbin.emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
        Ok(())
    }
}

/// Because GStreamer's WebRTCBin often crashes when receiving an invalid SDP,
/// we use Mozzila's SDP parser to manipulate the SDP Message before giving it to GStreamer.
///
/// After the text round-trip we also run an allowlist/blocklist pass on
/// attributes that have historically confused webrtcbin / libnice /
/// the DTLS negotiator on *incoming* descriptions. Browsers have shipped
/// SDPs that aborted the whole GStreamer process from inside
/// `set-remote-description` with no Rust-level backtrace, so the defense
/// has to be as conservative as possible here.
#[instrument(level = "debug", skip_all)]
fn sanitize_sdp(sdp: &gst_webrtc::WebRTCSessionDescription) -> Result<gst_sdp::SDPMessage> {
    let mut new_sdp = gst_sdp::SDPMessage::parse_buffer(
        webrtc_sdp::parse_sdp(sdp.sdp().as_text()?.as_str(), false)?
            .to_string()
            .as_bytes(),
    )
    .map_err(anyhow::Error::msg)?;

    let is_answer = matches!(sdp.type_(), gst_webrtc::WebRTCSDPType::Answer);

    new_sdp.medias_mut().for_each(|media| {
        scrub_remote_media_attributes(media, is_answer);
    });

    Ok(new_sdp)
}

/// Drop attributes on an incoming remote SDP media section that are either
/// ambiguous given our own offer (direction conflicts) or outside the set
/// we negotiate. This is intentionally conservative: we only remove items
/// we can prove are safe to drop, and we never add anything.
fn scrub_remote_media_attributes(media: &mut gst_sdp::SDPMediaRef, is_answer: bool) {
    let mut to_remove: Vec<usize> = Vec::new();
    let mut seen_rtcp_fb: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_extmap_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (idx, attr) in media.attributes().enumerate() {
        let key = attr.key();
        let value = attr.value().unwrap_or_default();

        match key {
            // Our offer is `sendonly`; a peer answering with `recvonly`
            // is semantically fine but past Chromium builds have emitted
            // it in ways that confuse webrtcbin's direction handling.
            // Strip it on incoming answers -- the transceiver direction
            // we set up on the sink pad is authoritative.
            "recvonly" if is_answer => {
                to_remove.push(idx);
            }
            // `sendonly` / `sendrecv` / `inactive` on an answer to our
            // sendonly offer would be a protocol violation; drop them so
            // webrtcbin doesn't try to reconcile conflicting directions.
            "sendonly" | "sendrecv" | "inactive" if is_answer => {
                to_remove.push(idx);
            }
            // Deduplicate rtcp-fb attributes -- some peers emit the same
            // feedback line twice which webrtcbin parses into duplicate
            // transceiver state.
            "rtcp-fb" if !seen_rtcp_fb.insert(value.to_string()) => {
                to_remove.push(idx);
            }
            // Deduplicate extmap by ID. We inject our own playout-delay
            // extension at a fixed ID on the *offer* side; a remote
            // re-declaration with the same ID but a different URI will
            // desynchronize the extension map.
            "extmap" => {
                let id = value.split_whitespace().next().unwrap_or("").to_string();
                if id.is_empty() || !seen_extmap_ids.insert(id) {
                    to_remove.push(idx);
                }
            }
            // `setup` must be one of "active", "passive", "actpass",
            // "holdconn" per RFC 4145. Anything else is a stray value
            // and must not be handed to the DTLS state machine.
            "setup" if !matches!(value, "active" | "passive" | "actpass" | "holdconn") => {
                warn!("Stripping non-standard a=setup:{value} from remote SDP");
                to_remove.push(idx);
            }
            _ => {}
        }
    }

    for idx in to_remove.into_iter().rev() {
        let _ = media.remove_attribute(idx as u32);
    }
}

/// Events the `connection-state` notify callback can forward to the
/// `FailSafeKiller` thread. `Connected` makes the killer stand down;
/// `Failed` asks it to terminate the session early instead of waiting
/// the full negotiation grace period.
enum PeerEvent {
    Connected,
    Failed,
}

/// Extract a best-effort string from a panic payload returned by
/// `std::panic::catch_unwind`. Falls back to a generic message if the
/// payload is not a string type.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn is_well_formed_profile_level_id(plid: &str) -> bool {
    plid.len() == 6 && plid.chars().all(|c| c.is_ascii_hexdigit())
}

#[instrument(level = "debug", skip_all)]
fn customize_sent_sdp(
    sdp: &gst_sdp::SDPMessageRef,
    session: Option<(&BindAnswer, &Arc<Mutex<SessionStats>>)>,
) -> Result<gst_sdp::SDPMessage> {
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
                    let retained_fmtp_fields = [
                        "profile-level-id",
                        "sprop-parameter-sets",
                        "sprop-vps",
                        "sprop-sps",
                        "sprop-pps",
                        "packetization-mode",
                        "level-asymmetry-allowed",
                        "profile-id",
                        "profile-space",
                        "tier-flag",
                        "level-id",
                        "profile-compatibility-indicator",
                        "interop-constraints",
                    ];
                    new_configs.retain(|config| {
                        let Some((key, value)) = config.split_once('=') else {
                            return false;
                        };
                        if key == "profile-level-id" {
                            return is_well_formed_profile_level_id(value);
                        }
                        retained_fmtp_fields.contains(&key)
                    });

                    trace!("fmtp attribute parsed: payload: {payload:?}, values: {new_configs:?}");

                    if encoding == "H264" {
                        let h264_fmtp_defaults = [
                            ("packetization-mode", "1"),
                            ("level-asymmetry-allowed", "1"),
                        ];
                        // IANA video/H264 defines these offer/answer fmtp
                        // parameters; video/H265 uses profile/tier/level
                        // fields instead.
                        // Reference: https://www.iana.org/assignments/media-types/video/H264
                        for (key, value) in h264_fmtp_defaults {
                            if !new_configs
                                .iter()
                                .any(|v| v.starts_with(&format!("{key}=")))
                            {
                                new_configs.push(format!("{key}={value}"));
                            }
                        }
                    }

                    let new_configs_str = new_configs.join(";");
                    let new_value = [payload, &new_configs_str].join(" ");

                    if value != new_value {
                        debug!(
                            session_id = ?session.map(|(bind, _)| bind.session_id),
                            encoding,
                            "Rewrote fmtp line in outgoing SDP\nfrom: {value}\nto:   {new_value}",
                        );
                    } else {
                        trace!(
                            session_id = ?session.map(|(bind, _)| bind.session_id),
                            encoding,
                            "Preserved fmtp line in outgoing SDP: {value}",
                        );
                    }

                    let new_fmtp_attribute = gst_sdp::SDPAttribute::new("fmtp", Some(&new_value));

                    if let Err(error) = media.replace_attribute(fmtp_idx as u32, new_fmtp_attribute)
                    {
                        warn!("fmtp customization failed: {error:?}");
                    }

                    trace!("fmtp attribute customized \nfrom: {value:?}\nto: {new_value:?}");
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

/// Install a BUFFER probe on the given src pad that writes the playout-delay
/// RTP header extension (min=0, max=0) into every outgoing RTP packet.
/// This tells the browser "render immediately, no smoothing buffer."
fn install_playout_delay_probe(src_pad: &gst::Pad) {
    src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
        if let Some(gst::PadProbeData::Buffer(ref mut buffer)) = info.data {
            let buffer = buffer.make_mut();
            if let Ok(mut rtp) = gst_rtp::RTPBuffer::from_buffer_writable(buffer) {
                let _ = rtp.add_extension_onebyte_header(PLAYOUT_DELAY_EXT_ID, &[0x00, 0x00, 0x00]);
            }
        }
        gst::PadProbeReturn::Ok
    });

    debug!("Playout-delay probe installed on pad (ext ID {PLAYOUT_DELAY_EXT_ID})");
}

/// Optimise the webrtcbin send path once `connection-state → Connected`:
///
/// 1. **Excise FEC/RED/RTX encoders** – `rtpulpfecenc`, `rtpredenc`, and
///    `rtprtxsend` are created even with `fec-type=None`; we surgically
///    unlink and remove them.
/// 2. **Disable sync on all internal sinks** – setting `sync=false` on
///    every element that exposes the property (clocksync pacing elements
///    AND transport sinks like nicesink) prevents clock-based packet pacing.
/// 3. **Remove the pre-excision BLOCK probe** – data is allowed to flow
///    through the proxysrc internal queue into webrtcbin now that the
///    FEC/RED/RTX elements are gone and the negotiated SDP is in effect.
///
/// All excision is done inside a `BLOCK_DOWNSTREAM` probe on the tee src
/// pad to guarantee no data races.
fn optimise_send_path(
    webrtcbin: &gst::Element,
    block_pad_slot: &Arc<Mutex<Option<glib::WeakRef<gst::Pad>>>>,
    block_probe_slot: &Arc<Mutex<Option<gst::PadProbeId>>>,
) {
    const EXCISABLE_PREFIXES: &[&str] = &["rtpulpfecenc", "rtpredenc", "rtprtxsend"];

    if let Some(bin) = webrtcbin.downcast_ref::<gst::Bin>() {
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

    // Release the pre-excision BLOCK on the proxysrc queue src pad. Doing
    // this last ensures all FEC/RED/RTX shaping happens before the first
    // buffer reaches webrtcbin.
    let pad = block_pad_slot
        .lock()
        .expect("block_pad mutex")
        .as_ref()
        .and_then(|w| w.upgrade());
    let probe_id = block_probe_slot.lock().expect("block_probe mutex").take();
    if let (Some(pad), Some(probe_id)) = (pad, probe_id) {
        pad.remove_probe(probe_id);
        debug!("Removed pre-excision BLOCK probe from proxysrc queue src pad");
    }

    if crate::cli::manager::is_dot_enabled()
        && let Some(bin) = webrtcbin.downcast_ref::<gst::Bin>()
    {
        crate::stream::gst::utils::dump_bin_elements(bin, "WebRTCBin internals");
    }
}

/// Send a ForceKeyUnit event upstream so the encoder produces a fresh keyframe
/// right after the WebRTC peer connects.
fn send_force_key_unit_upstream(webrtcbin: &gst::Element) {
    let fku_event = gst_video::UpstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
    // Request pads (sink_%u) are not returned by static_pad();
    // iterate all sink pads and pick the first one with a peer. The peer
    // is the proxysrc src pad inside the session sub-pipeline, and the
    // event propagates upstream through the proxy bridge to the producer
    // pipeline's encoder.
    let fku_pad = webrtcbin
        .iterate_sink_pads()
        .into_iter()
        .filter_map(Result::ok)
        .find_map(|pad| pad.peer());
    let _ = fku_pad.as_ref().map(|p| p.send_event(fku_event));
    debug!("Sent ForceKeyUnit upstream on WebRTC session connect");
}
/// Classification of an ICE candidate, mirroring the RFC 5245/8445 `typ`
/// values so the log pipeline can keep a small histogram per session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum IceCandidateKind {
    Host,
    ServerReflexive,
    PeerReflexive,
    Relay,
    #[default]
    Unknown,
}

impl IceCandidateKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::ServerReflexive => "srflx",
            Self::PeerReflexive => "prflx",
            Self::Relay => "relay",
            Self::Unknown => "unknown",
        }
    }
}

/// Per-session histogram of ICE candidates we have seen, split by kind.
#[derive(Debug, Clone, Copy, Default)]
pub struct IceCandidateCounts {
    pub host: u32,
    pub srflx: u32,
    pub prflx: u32,
    pub relay: u32,
    pub unknown: u32,
}

impl IceCandidateCounts {
    pub fn total(&self) -> u32 {
        self.host + self.srflx + self.prflx + self.relay + self.unknown
    }

    fn increment(&mut self, kind: IceCandidateKind) {
        match kind {
            IceCandidateKind::Host => self.host += 1,
            IceCandidateKind::ServerReflexive => self.srflx += 1,
            IceCandidateKind::PeerReflexive => self.prflx += 1,
            IceCandidateKind::Relay => self.relay += 1,
            IceCandidateKind::Unknown => self.unknown += 1,
        }
    }
}

/// Classify an ICE candidate SDP string by its `typ` token.
///
/// Accepts either the full `a=candidate:...` attribute line or the
/// bare candidate value (what webrtcbin emits on `on-ice-candidate`).
fn classify_ice_candidate(candidate: &str) -> IceCandidateKind {
    let mut tokens = candidate.split_ascii_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "typ" {
            return match tokens.next() {
                Some("host") => IceCandidateKind::Host,
                Some("srflx") => IceCandidateKind::ServerReflexive,
                Some("prflx") => IceCandidateKind::PeerReflexive,
                Some("relay") => IceCandidateKind::Relay,
                _ => IceCandidateKind::Unknown,
            };
        }
    }
    IceCandidateKind::Unknown
}

/// Per-session observability counters updated from the many GStreamer /
/// libnice callbacks that run during a WebRTC session. Fields default to
/// `None` / zero; they are filled in as the session progresses. All
/// timestamps are wall-clock-ish `Instant`s measured from session start.
#[derive(Debug)]
pub struct SessionStats {
    pub started_at: std::time::Instant,
    pub offer_sent_at: Option<std::time::Instant>,
    pub answer_received_at: Option<std::time::Instant>,
    pub first_client_ice_at: Option<std::time::Instant>,
    pub connected_at: Option<std::time::Instant>,
    pub first_frame_at: Option<std::time::Instant>,
    pub local_ice: IceCandidateCounts,
    pub remote_ice: IceCandidateCounts,
    pub peer_connection_state: Option<gst_webrtc::WebRTCPeerConnectionState>,
    pub ice_connection_state: Option<gst_webrtc::WebRTCICEConnectionState>,
    pub ice_gathering_state: Option<gst_webrtc::WebRTCICEGatheringState>,
    pub upstream_encoding: Option<String>,
    pub end_reason: Option<String>,
}

impl Default for SessionStats {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStats {
    pub fn new() -> Self {
        Self {
            started_at: std::time::Instant::now(),
            offer_sent_at: None,
            answer_received_at: None,
            first_client_ice_at: None,
            connected_at: None,
            first_frame_at: None,
            local_ice: IceCandidateCounts::default(),
            remote_ice: IceCandidateCounts::default(),
            peer_connection_state: None,
            ice_connection_state: None,
            ice_gathering_state: None,
            upstream_encoding: None,
            end_reason: None,
        }
    }

    /// Heuristic label for the most likely reason a session never
    /// reached `Connected`. Used by the FailSafeKiller to turn a bare
    /// "timeout after 30 s" warning into actionable context.
    pub fn likely_cause_at_negotiation_timeout(&self) -> &'static str {
        if self.offer_sent_at.is_none() {
            return "offer never sent (webrtcbin did not emit on-negotiation-needed)";
        }
        if self.answer_received_at.is_none() {
            return "client did not reply with SDP answer";
        }
        if self.remote_ice.total() == 0 {
            return "client sent SDP answer but no ICE candidates (peer-to-peer reachability failure on client side)";
        }
        if self.local_ice.total() == 0 {
            return "no local ICE candidates gathered (libnice / network interface issue)";
        }
        if self.local_ice.srflx == 0 && self.local_ice.relay == 0 {
            return "only host candidates gathered (STUN/TURN unreachable or no public reflexive addr)";
        }
        if self.connected_at.is_none() {
            return "ICE checks did not succeed (mismatched candidates / DTLS failure)";
        }
        "unknown"
    }
}

/// Resolve the host portion of a STUN / TURN URL to a socket address so
/// we can detect configuration typos or broken DNS before the session
/// fails silently inside libnice.
fn resolve_webrtc_endpoint(url: &str) -> Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;

    let parsed = url::Url::parse(url).context("failed to parse STUN/TURN url")?;
    let host = parsed
        .host_str()
        .context("STUN/TURN url has no host component")?;
    let port = parsed.port().unwrap_or(3478);

    (host, port)
        .to_socket_addrs()
        .context("failed to resolve STUN/TURN host")?
        .next()
        .context("STUN/TURN resolution returned no addresses")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal offer SDP shaped like what `webrtcbin` emits for
    /// a single codec media line. `encoding` is the codec name as it
    /// appears in `a=rtpmap:96 <encoding>/90000` (e.g. `"H264"` or
    /// `"H265"`). `fmtp_body` controls the fmtp line for pt 96:
    /// `Some("...")` produces `a=fmtp:96 ...`, `None` omits the line.
    fn sdp_with_encoded_fmtp(encoding: &str, fmtp_body: Option<&str>) -> gst_sdp::SDPMessage {
        gst::init().unwrap();

        let fmtp_line = match fmtp_body {
            Some(body) => format!("a=fmtp:96 {body}\r\n"),
            None => String::new(),
        };
        let text = format!(
            "v=0\r\n\
             o=- 0 0 IN IP4 127.0.0.1\r\n\
             s=-\r\n\
             t=0 0\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
             c=IN IP4 0.0.0.0\r\n\
             a=rtpmap:96 {encoding}/90000\r\n\
             {fmtp_line}"
        );
        gst_sdp::SDPMessage::parse_buffer(text.as_bytes()).unwrap()
    }

    /// Return the raw value of the first `a=fmtp:...` attribute (the part
    /// after the `:`, starting with the payload type), or `None` if absent.
    fn fmtp_value_of(sdp: &gst_sdp::SDPMessage) -> Option<String> {
        sdp.media(0)?
            .attributes()
            .find(|a| a.key() == "fmtp")
            .and_then(|a| a.value().map(|s| s.to_string()))
    }

    /// Parse a specific `<key>=...` entry out of an fmtp value of the form
    /// `"<pt> key=val;key=val;..."`. Used for both `profile-level-id`
    /// (H.264) and `profile-id`, `profile-space`, `tier-flag` (H.265).
    fn fmtp_field(fmtp_value: &str, key: &str) -> Option<String> {
        let (_pt, configs) = fmtp_value.split_once(' ')?;
        let prefix = format!("{key}=");
        configs
            .split(';')
            .find_map(|kv| kv.trim().strip_prefix(prefix.as_str()))
            .map(|s| s.to_string())
    }

    /// The RTP caps observed on the tee sink pad carry the camera's true
    /// H.264 profile (e.g. Main 4.1, `4d4029` in the JM log). If those
    /// caps are fed through unchanged, `customize_sent_sdp` must preserve
    /// the profile-level-id so the browser's decoder matches the wire.
    #[test]
    fn customize_sent_sdp_preserves_h264_main_profile() {
        let input = sdp_with_encoded_fmtp(
            "H264",
            Some(
                "profile-level-id=4d4029;packetization-mode=1;\
                 sprop-parameter-sets=Z01AKZZUA8ARPyo=,aO44gA==;level-asymmetry-allowed=1",
            ),
        );

        let output = customize_sent_sdp(&input, None).expect("customize_sent_sdp should succeed");

        let fmtp = fmtp_value_of(&output).expect("fmtp should be present in output");
        let plid = fmtp_field(&fmtp, "profile-level-id")
            .expect("profile-level-id should be present in output fmtp");

        assert_eq!(
            plid, "4d4029",
            "expected Main 4.1 profile-level-id to be preserved, got {plid:?} in fmtp {fmtp:?}",
        );
    }

    /// High profile cameras (`64xxxx`) must not be squashed to Constrained
    /// Baseline (`42e0..`); the browser then cannot decode the wire stream.
    #[test]
    fn customize_sent_sdp_preserves_h264_high_profile() {
        let input =
            sdp_with_encoded_fmtp("H264", Some("profile-level-id=640028;packetization-mode=1"));

        let output = customize_sent_sdp(&input, None).expect("customize_sent_sdp should succeed");

        let fmtp = fmtp_value_of(&output).expect("fmtp should be present in output");
        let plid = fmtp_field(&fmtp, "profile-level-id")
            .expect("profile-level-id should be present in output fmtp");

        assert_eq!(
            plid, "640028",
            "expected High profile-level-id to be preserved, got {plid:?} in fmtp {fmtp:?}",
        );
    }

    /// H.265 fmtp carries `profile-id`, `profile-space`, and `tier-flag`
    /// that together identify the decoder profile (Main, Main-10, etc.).
    #[test]
    fn customize_sent_sdp_preserves_h265_profile_fields() {
        let input = sdp_with_encoded_fmtp(
            "H265",
            Some(
                "level-id=120;profile-space=0;profile-id=1;tier-flag=0;\
                 profile-compatibility-indicator=6;interop-constraints=B00000000000;\
                 sprop-vps=QAEMAf//AWAAAAMAgAAAAwAAAwB4LA==;\
                 sprop-sps=QgEBAWAAAAMAgAAAAwAAAwB4oAKAgC0WXmbkkmbAgAAH0AAAHUwQ;\
                 sprop-pps=RAHA8vA8kA==",
            ),
        );

        let output = customize_sent_sdp(&input, None).expect("customize_sent_sdp should succeed");

        let fmtp = fmtp_value_of(&output).expect("fmtp should be present in output");

        assert_eq!(
            fmtp_field(&fmtp, "profile-id").as_deref(),
            Some("1"),
            "expected H.265 Main profile-id=1 to be preserved, got fmtp {fmtp:?}",
        );
        assert_eq!(
            fmtp_field(&fmtp, "profile-space").as_deref(),
            Some("0"),
            "expected H.265 profile-space=0 to be preserved, got fmtp {fmtp:?}",
        );
        assert_eq!(
            fmtp_field(&fmtp, "tier-flag").as_deref(),
            Some("0"),
            "expected H.265 tier-flag=0 to be preserved, got fmtp {fmtp:?}",
        );
    }

    /// When `webrtcbin` emits no fmtp for the payload (e.g. because
    /// `codec-preferences` carried no codec-specific fields), the rewrite
    /// must not fabricate one. Otherwise we advertise a profile the
    /// upstream stream never produced. Covers both H.264 and H.265 since
    /// the fabrication path is codec-independent.
    #[test]
    fn customize_sent_sdp_does_not_fabricate_fmtp_when_absent() {
        for encoding in ["H264", "H265"] {
            let input = sdp_with_encoded_fmtp(encoding, None);

            let output =
                customize_sent_sdp(&input, None).expect("customize_sent_sdp should succeed");

            assert!(
                fmtp_value_of(&output).is_none(),
                "encoding {encoding}: expected no fmtp in output when input had none, got {:?}",
                fmtp_value_of(&output),
            );
        }
    }

    /// Safety net for the `original_plid.get(4..6)` slice: even on
    /// malformed input (here a deliberately short profile-level-id), the
    /// rewrite must never emit a non-hex or wrong-length profile-level-id
    /// that would crash or confuse the browser.
    #[test]
    fn customize_sent_sdp_emits_well_formed_profile_level_id() {
        let input = sdp_with_encoded_fmtp("H264", Some("profile-level-id=42;packetization-mode=1"));

        let output = customize_sent_sdp(&input, None).expect("customize_sent_sdp should succeed");

        let Some(fmtp) = fmtp_value_of(&output) else {
            return;
        };
        let Some(plid) = fmtp_field(&fmtp, "profile-level-id") else {
            return;
        };

        assert!(
            plid.len() == 6 && plid.chars().all(|c| c.is_ascii_hexdigit()),
            "profile-level-id must be 6 hex chars, got {plid:?} in fmtp {fmtp:?}",
        );
    }
}
