use anyhow::{anyhow, Context, Result};
use async_tungstenite::tungstenite;
use futures::{SinkExt, StreamExt};
use gst::prelude::*;
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;

use super::protocol::*;
use super::{attach_frame_probe, SampleSender};

enum SignalOutgoing {
    SdpAnswer(String),
    IceCandidate {
        sdp_m_line_index: u32,
        candidate: String,
    },
}

/// Receive the next text-based signaling message, skipping pings/pongs.
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

/// Send a protocol message as JSON over the WebSocket.
async fn send_msg(
    ws: &mut (impl futures::Sink<tungstenite::Message, Error = tungstenite::Error> + Unpin),
    msg: impl Into<Protocol>,
) -> Result<()> {
    let json = serde_json::to_string(&Into::<Protocol>::into(msg))?;
    ws.send(tungstenite::Message::Text(json)).await?;
    Ok(())
}

pub async fn create_webrtc_client(
    name: &str,
    signalling_url: &str,
    producer_id: Option<Uuid>,
    sample_sender: SampleSender,
) -> Result<(gst::Pipeline, tokio::task::JoinHandle<Result<()>>)> {
    let pipeline = gst::Pipeline::with_name(name);
    let client_name = name.to_string();

    let webrtcbin = gst::ElementFactory::make("webrtcbin")
        .property("bundle-policy", gst_webrtc::WebRTCBundlePolicy::MaxBundle)
        .property("latency", 0u32)
        .build()
        .context("Failed to create webrtcbin")?;

    pipeline.add(&webrtcbin)?;

    // Channel: GStreamer callbacks → signaling task
    let (gst_tx, gst_rx) = tokio_mpsc::unbounded_channel::<SignalOutgoing>();

    // Connect on-ice-candidate: forward candidates to the signaling task
    let gst_tx_ice = gst_tx.clone();
    webrtcbin.connect("on-ice-candidate", false, move |values| {
        let sdp_m_line_index = values[1].get::<u32>().expect("Invalid argument");
        let candidate = values[2].get::<String>().expect("Invalid argument");
        let _ = gst_tx_ice.send(SignalOutgoing::IceCandidate {
            sdp_m_line_index,
            candidate,
        });
        None
    });

    // Connect pad-added: link incoming media to depay → parse → capsfilter → fakesink with probe
    let pipeline_weak = pipeline.downgrade();
    let client_name_pad = client_name.clone();
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

        let Ok(depay) = gst::ElementFactory::make(depay_factory).build() else {
            eprintln!("[{client_name_pad}] Failed to create {depay_factory}");
            return;
        };
        let Ok(parse) = gst::ElementFactory::make(parse_factory).build() else {
            eprintln!("[{client_name_pad}] Failed to create {parse_factory}");
            return;
        };
        let Ok(capsfilter) = gst::ElementFactory::make("capsfilter")
            .property("caps", &norm_caps)
            .build()
        else {
            return;
        };
        let Ok(sink) = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
        else {
            return;
        };

        pipeline.add_many([&depay, &parse, &capsfilter, &sink]).ok();
        gst::Element::link_many([&depay, &parse, &capsfilter, &sink]).ok();
        depay.sync_state_with_parent().ok();
        parse.sync_state_with_parent().ok();
        capsfilter.sync_state_with_parent().ok();
        sink.sync_state_with_parent().ok();

        let probe_pad = parse.static_pad("src").unwrap();
        attach_frame_probe(&probe_pad, client_name_pad.clone(), sample_sender.clone());

        let sink_pad = depay.static_pad("sink").unwrap();
        if pad.link(&sink_pad).is_err() {
            eprintln!("[{client_name_pad}] Failed to link webrtcbin pad to depayloader");
        }
    });

    // --- WebSocket signaling handshake (sequential) ---

    let (ws_stream, _) = async_tungstenite::tokio::connect_async(signalling_url)
        .await
        .context("Failed to connect to signalling server")?;
    let (mut ws_sink, mut ws_source) = ws_stream.split();

    // Step 1: request consumer ID
    send_msg(&mut ws_sink, Question::PeerId).await?;
    let consumer_id = loop {
        match recv_msg(&mut ws_source).await? {
            Message::Answer(Answer::PeerId(p)) => break p.id,
            _ => continue,
        }
    };
    eprintln!("[{client_name}] Consumer ID: {consumer_id}");

    // Step 2: request available streams
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
                        "Multiple streams available, specify --producer-id. Available:\n{}",
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
    eprintln!("[{client_name}] Target producer: {target_producer_id}");

    // Step 3: start session (buffer any early negotiation messages)
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
    eprintln!("[{client_name}] Session started: {}", bind.session_id);

    // --- Spawn ongoing signaling task ---

    let webrtcbin_clone = webrtcbin.clone();
    let gst_tx_answer = gst_tx.clone();
    let bind_clone = bind.clone();

    let signaling_task = tokio::spawn(async move {
        let mut gst_rx = gst_rx;

        // Process any messages that arrived between StartSession answer and now
        for msg in early_messages {
            handle_incoming(&msg, &webrtcbin_clone, &gst_tx_answer, &client_name);
        }

        loop {
            tokio::select! {
                ws_msg = ws_source.next() => {
                    let Some(msg) = ws_msg else { break };
                    let msg = msg?;
                    match msg {
                        tungstenite::Message::Text(text) => {
                            let proto: Protocol = serde_json::from_str(&text)?;
                            let should_stop = handle_incoming(
                                &proto.message,
                                &webrtcbin_clone,
                                &gst_tx_answer,
                                &client_name,
                            );
                            if should_stop {
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
                            send_msg(&mut ws_sink, neg).await?;
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
                            send_msg(&mut ws_sink, neg).await?;
                        }
                    }
                }
            }
        }

        Ok(())
    });

    Ok((pipeline, signaling_task))
}

/// Process a single incoming signaling message.
/// Returns `true` if the session was ended by the server.
fn handle_incoming(
    msg: &Message,
    webrtcbin: &gst::Element,
    gst_tx: &tokio_mpsc::UnboundedSender<SignalOutgoing>,
    client_name: &str,
) -> bool {
    match msg {
        Message::Negotiation(Negotiation::MediaNegotiation(neg)) => {
            if let RTCSessionDescription::Offer(ref sdp) = neg.sdp {
                handle_sdp_offer(webrtcbin, &sdp.sdp, gst_tx, client_name);
            }
        }
        Message::Negotiation(Negotiation::IceNegotiation(neg)) => {
            if let Some(ref candidate) = neg.ice.candidate {
                let idx = neg.ice.sdp_m_line_index.unwrap_or(0);
                webrtcbin.emit_by_name::<()>("add-ice-candidate", &[&idx, &candidate.as_str()]);
            }
        }
        Message::Question(Question::EndSession(end)) => {
            eprintln!("[{client_name}] Session ended by server: {}", end.reason);
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
    client_name: &str,
) {
    let sdp_msg = match gst_sdp::SDPMessage::parse_buffer(sdp_text.as_bytes()) {
        Ok(msg) => msg,
        Err(e) => {
            eprintln!("[{client_name}] Failed to parse SDP: {e:?}");
            return;
        }
    };

    let offer =
        gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, sdp_msg);

    webrtcbin.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

    let webrtcbin_weak = webrtcbin.downgrade();
    let tx = gst_tx.clone();
    let name = client_name.to_string();

    let promise = gst::Promise::with_change_func(move |reply| {
        let reply = match reply {
            Ok(Some(reply)) => reply,
            Ok(None) => {
                eprintln!("[{name}] Answer creation got no response");
                return;
            }
            Err(e) => {
                eprintln!("[{name}] Answer creation failed: {e:?}");
                return;
            }
        };

        let answer = match reply.get_optional::<gst_webrtc::WebRTCSessionDescription>("answer") {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                eprintln!("[{name}] No \"answer\" in create-answer reply");
                return;
            }
            Err(e) => {
                eprintln!("[{name}] Failed to get answer: {e:?}");
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
                eprintln!("[{name}] Failed to serialize answer SDP: {e:?}");
            }
        }
    });

    webrtcbin.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
}

/// Returns (depay_factory, parse_factory, normalized_caps) for the codec detected on the pad.
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
