use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_tungstenite::tungstenite;
use futures::{SinkExt, StreamExt};
use gst::prelude::*;
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;

use crate::{attach_frame_probe, protocol::*, SampleSender, StreamClient};

pub struct WebrtcClient {
    pipeline: gst::Pipeline,
    frame_count: Arc<AtomicU64>,
    decoded_count: Arc<AtomicU64>,
    _signaling_handle: tokio::task::JoinHandle<()>,
}

#[async_trait::async_trait]
impl StreamClient for WebrtcClient {
    fn frames(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }
}

impl WebrtcClient {
    pub async fn connect(
        signalling_url: &str,
        producer_id: Option<Uuid>,
        sender: Option<SampleSender>,
        ice_filter: Option<&str>,
    ) -> Result<Self> {
        gst::init()?;

        let pipeline = gst::Pipeline::with_name("webrtc-client");
        let frame_count = Arc::new(AtomicU64::new(0));
        let decoded_count = Arc::new(AtomicU64::new(0));

        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .property("bundle-policy", gst_webrtc::WebRTCBundlePolicy::MaxBundle)
            .property("latency", 0u32)
            .build()
            .context("Failed to create webrtcbin")?;
        pipeline.add(&webrtcbin)?;

        let (gst_tx, gst_rx) = tokio_mpsc::unbounded_channel::<SignalOutgoing>();

        let gst_tx_ice = gst_tx.clone();
        let outbound_filter = ice_filter.map(String::from);
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            let sdp_m_line_index = values[1].get::<u32>().expect("Invalid argument");
            let candidate = values[2].get::<String>().expect("Invalid argument");
            if let Some(ref prefix) = outbound_filter {
                let stripped = candidate.strip_prefix("candidate:").unwrap_or(&candidate);
                let parts: Vec<&str> = stripped.split_whitespace().collect();
                let ip = parts.get(4).copied().unwrap_or("");
                if !ip.starts_with(prefix.as_str()) {
                    return None;
                }
            }
            let _ = gst_tx_ice.send(SignalOutgoing::IceCandidate {
                sdp_m_line_index,
                candidate,
            });
            None
        });

        let pipeline_weak = pipeline.downgrade();
        let counter = Arc::clone(&frame_count);
        let dec_counter = Arc::clone(&decoded_count);
        webrtcbin.connect_pad_added(move |_webrtcbin, pad| {
            if pad.direction() != gst::PadDirection::Src {
                return;
            }

            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };

            let (depay_factory, parse_factory, norm_caps) = detect_codec_elements(pad).unwrap_or((
                "rtph264depay",
                "h264parse",
                gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            ));

            let decoder_factory = if parse_factory.contains("h265") {
                "avdec_h265"
            } else {
                "avdec_h264"
            };

            let Ok(depay) = gst::ElementFactory::make(depay_factory).build() else {
                eprintln!("[webrtc-client] Failed to create {depay_factory}");
                return;
            };
            let Ok(parse) = gst::ElementFactory::make(parse_factory).build() else {
                eprintln!("[webrtc-client] Failed to create {parse_factory}");
                return;
            };
            let Ok(capsfilter) = gst::ElementFactory::make("capsfilter")
                .property("caps", &norm_caps)
                .build()
            else {
                return;
            };
            let Ok(decoder) = gst::ElementFactory::make(decoder_factory).build() else {
                eprintln!("[webrtc-client] Failed to create {decoder_factory}");
                return;
            };
            let Ok(sink) = gst::ElementFactory::make("fakesink")
                .property("sync", false)
                .property("async", false)
                .build()
            else {
                return;
            };

            pipeline
                .add_many([&depay, &parse, &capsfilter, &decoder, &sink])
                .ok();
            gst::Element::link_many([&depay, &parse, &capsfilter, &decoder, &sink]).ok();
            depay.sync_state_with_parent().ok();
            parse.sync_state_with_parent().ok();
            capsfilter.sync_state_with_parent().ok();
            decoder.sync_state_with_parent().ok();
            sink.sync_state_with_parent().ok();

            let probe_pad = parse.static_pad("src").unwrap();
            let ctr = counter.clone();
            probe_pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                ctr.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });

            if let Some(ref sender) = sender {
                let probe_pad = parse.static_pad("src").unwrap();
                attach_frame_probe(&probe_pad, "webrtc-client".to_string(), sender.clone());
            }

            let dec_src = decoder.static_pad("src").unwrap();
            let dc = dec_counter.clone();
            dec_src.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                dc.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });

            let sink_pad = depay.static_pad("sink").unwrap();
            if pad.link(&sink_pad).is_err() {
                eprintln!("[webrtc-client] Failed to link webrtcbin pad to depayloader");
            }
        });

        let ws = {
            let max_attempts = 5;
            let mut last_err: Option<tungstenite::Error> = None;
            let mut conn = None;
            for attempt in 0..max_attempts {
                match async_tungstenite::tokio::connect_async(signalling_url).await {
                    Ok((ws, _)) => {
                        conn = Some(ws);
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "[webrtc-client] ws connect attempt {}/{max_attempts} failed: {e}, retrying...",
                            attempt + 1
                        );
                        last_err = Some(e);
                        if attempt + 1 < max_attempts {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }
            conn.ok_or_else(|| last_err.unwrap())
                .context("ws connect failed after retries")?
        };
        let (mut ws_sink, mut ws_source) = ws.split();

        send_msg(&mut ws_sink, Question::PeerId).await?;
        let consumer_id = loop {
            match recv_msg(&mut ws_source).await? {
                Message::Answer(Answer::PeerId(p)) => break p.id,
                _ => continue,
            }
        };

        send_msg(&mut ws_sink, Question::AvailableStreams).await?;
        let target_producer_id = loop {
            match recv_msg(&mut ws_source).await? {
                Message::Answer(Answer::AvailableStreams(streams)) => {
                    let target = if let Some(pid) = producer_id {
                        pid
                    } else if streams.len() == 1 {
                        streams[0].id
                    } else {
                        return Err(anyhow!(
                            "Multiple streams available, specify producer_id. Available:\n{}",
                            streams
                                .iter()
                                .map(|s| format!("  {} ({})", s.name, s.id))
                                .collect::<Vec<_>>()
                                .join("\n")
                        ));
                    };
                    break target;
                }
                _ => continue,
            }
        };

        send_msg(
            &mut ws_sink,
            Question::StartSession(BindOffer {
                consumer_id,
                producer_id: target_producer_id,
            }),
        )
        .await?;

        let mut early_messages = Vec::new();
        let bind = loop {
            match recv_msg(&mut ws_source).await? {
                Message::Answer(Answer::StartSession(bind)) => break bind,
                other => early_messages.push(other),
            }
        };

        let webrtcbin_clone = webrtcbin.clone();
        let gst_tx_answer = gst_tx.clone();
        let bind_clone = bind.clone();
        let inbound_filter = ice_filter.map(String::from);

        let handle = tokio::spawn(async move {
            let mut gst_rx = gst_rx;

            for msg in early_messages {
                handle_incoming(
                    &msg,
                    &webrtcbin_clone,
                    &gst_tx_answer,
                    inbound_filter.as_deref(),
                );
            }

            loop {
                tokio::select! {
                    ws_msg = ws_source.next() => {
                        let Some(Ok(msg)) = ws_msg else { break };
                        match msg {
                            tungstenite::Message::Text(text) => {
                                let Ok(proto) = serde_json::from_str::<Protocol>(&text) else {
                                    continue;
                                };
                                if handle_incoming(
                                    &proto.message,
                                    &webrtcbin_clone,
                                    &gst_tx_answer,
                                    inbound_filter.as_deref(),
                                ) {
                                    break;
                                }
                            }
                            tungstenite::Message::Close(_) => break,
                            _ => {}
                        }
                    }

                    gst_msg = gst_rx.recv() => {
                        let Some(msg) = gst_msg else { break };
                        match msg {
                            SignalOutgoing::SdpAnswer(sdp_text) => {
                                let neg = MediaNegotiation {
                                    bind: bind_clone.clone(),
                                    sdp: RTCSessionDescription::Answer(Sdp { sdp: sdp_text }),
                                };
                                let _ = send_msg(&mut ws_sink, neg).await;
                            }
                            SignalOutgoing::IceCandidate { sdp_m_line_index, candidate } => {
                                let neg = IceNegotiation {
                                    bind: bind_clone.clone(),
                                    ice: RTCIceCandidateInit {
                                        candidate: Some(candidate),
                                        sdp_mid: None,
                                        sdp_m_line_index: Some(sdp_m_line_index),
                                        username_fragment: None,
                                    },
                                };
                                let _ = send_msg(&mut ws_sink, neg).await;
                            }
                        }
                    }
                }
            }
        });

        pipeline.set_state(gst::State::Playing)?;

        Ok(Self {
            pipeline,
            frame_count,
            decoded_count,
            _signaling_handle: handle,
        })
    }

    pub fn decoded_frames(&self) -> u64 {
        self.decoded_count.load(Ordering::Relaxed)
    }

    pub async fn wait_for_decoded_frames(&self, min: u64, timeout: Duration) -> Result<u64> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let n = self.decoded_frames();
            if n >= min {
                return Ok(n);
            }
            if tokio::time::Instant::now() > deadline {
                let parsed = self.frames();
                anyhow::bail!("only got {n} decoded frames (wanted {min}), parsed={parsed}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for WebrtcClient {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self._signaling_handle.abort();
    }
}

fn handle_incoming(
    msg: &Message,
    webrtcbin: &gst::Element,
    gst_tx: &tokio_mpsc::UnboundedSender<SignalOutgoing>,
    ice_filter: Option<&str>,
) -> bool {
    match msg {
        Message::Negotiation(Negotiation::MediaNegotiation(neg)) => {
            if let RTCSessionDescription::Offer(ref sdp) = neg.sdp {
                handle_sdp_offer(webrtcbin, &sdp.sdp, gst_tx);
            }
        }
        Message::Negotiation(Negotiation::IceNegotiation(neg)) => {
            if let Some(ref candidate) = neg.ice.candidate {
                let accept = match ice_filter {
                    Some(prefix) => {
                        let stripped = candidate.strip_prefix("candidate:").unwrap_or(candidate);
                        let parts: Vec<&str> = stripped.split_whitespace().collect();
                        let ip = parts.get(4).copied().unwrap_or("");
                        ip.starts_with(prefix)
                    }
                    None => true,
                };
                if accept {
                    let idx = neg.ice.sdp_m_line_index.unwrap_or(0);
                    webrtcbin.emit_by_name::<()>("add-ice-candidate", &[&idx, &candidate.as_str()]);
                }
            }
        }
        Message::Question(Question::EndSession(end)) => {
            eprintln!("[webrtc-client] Session ended by server: {}", end.reason);
            return true;
        }
        _ => {}
    }
    false
}

fn handle_sdp_offer(
    webrtcbin: &gst::Element,
    sdp_text: &str,
    gst_tx: &tokio_mpsc::UnboundedSender<SignalOutgoing>,
) {
    let sdp_msg = match gst_sdp::SDPMessage::parse_buffer(sdp_text.as_bytes()) {
        Ok(msg) => msg,
        Err(e) => {
            eprintln!("[webrtc-client] Failed to parse SDP: {e:?}");
            return;
        }
    };

    let offer =
        gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp_msg);

    webrtcbin.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

    let webrtcbin_weak = webrtcbin.downgrade();
    let tx = gst_tx.clone();

    let promise = gst::Promise::with_change_func(move |reply| {
        let reply = match reply {
            Ok(Some(reply)) => reply,
            Ok(None) => {
                eprintln!("[webrtc-client] Answer creation got no response");
                return;
            }
            Err(e) => {
                eprintln!("[webrtc-client] Answer creation failed: {e:?}");
                return;
            }
        };

        let answer = match reply.get_optional::<gst_webrtc::WebRTCSessionDescription>("answer") {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                eprintln!("[webrtc-client] No \"answer\" in create-answer reply");
                return;
            }
            Err(e) => {
                eprintln!("[webrtc-client] Failed to get answer: {e:?}");
                return;
            }
        };

        if let Some(webrtcbin) = webrtcbin_weak.upgrade() {
            webrtcbin
                .emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);
        }

        match answer.sdp().as_text() {
            Ok(sdp_text) => {
                let _ = tx.send(SignalOutgoing::SdpAnswer(sdp_text));
            }
            Err(e) => {
                eprintln!("[webrtc-client] Failed to serialize answer SDP: {e:?}");
            }
        }
    });

    webrtcbin.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
}

fn detect_codec_elements(pad: &gst::Pad) -> Option<(&'static str, &'static str, gst::Caps)> {
    let caps = pad.current_caps().or_else(|| {
        let query_caps = pad.query_caps(None);
        if query_caps.is_any() || query_caps.is_empty() {
            None
        } else {
            Some(query_caps)
        }
    })?;

    let structure = caps.structure(0)?;

    let codec = if let Ok(encoding) = structure.get::<&str>("encoding-name") {
        encoding.to_uppercase()
    } else {
        let name = structure.name().as_str();
        if name.contains("h264") {
            "H264".to_string()
        } else if name.contains("h265") {
            "H265".to_string()
        } else {
            return None;
        }
    };

    match codec.as_str() {
        "H264" => Some((
            "rtph264depay",
            "h264parse",
            gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        )),
        "H265" => Some((
            "rtph265depay",
            "h265parse",
            gst::Caps::builder("video/x-h265")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        )),
        _ => None,
    }
}

enum SignalOutgoing {
    SdpAnswer(String),
    IceCandidate {
        sdp_m_line_index: u32,
        candidate: String,
    },
}

async fn recv_msg(
    ws: &mut (impl futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin),
) -> Result<Message> {
    loop {
        let msg = ws.next().await.context("WebSocket closed")??;
        match msg {
            tungstenite::Message::Text(text) => {
                let proto: Protocol = serde_json::from_str(&text)?;
                return Ok(proto.message);
            }
            tungstenite::Message::Close(_) => return Err(anyhow!("WebSocket closed by server")),
            _ => continue,
        }
    }
}

async fn send_msg(
    ws: &mut (impl futures::Sink<tungstenite::Message, Error = tungstenite::Error> + Unpin),
    msg: impl Into<Protocol>,
) -> Result<()> {
    let json = serde_json::to_string(&Into::<Protocol>::into(msg))?;
    ws.send(tungstenite::Message::Text(json)).await?;
    Ok(())
}
