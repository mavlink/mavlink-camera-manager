use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use url::Url;

use super::types::*;

pub struct McmClient {
    client: reqwest::Client,
    base_url: String,
}

/// Records stream-state transitions in the background so tests can assert
/// that certain states were (or were never) visited.
pub struct StateMonitor {
    handle: tokio::task::JoinHandle<()>,
    transitions: Arc<Mutex<Vec<(std::time::Instant, StreamStatusState)>>>,
}

impl StateMonitor {
    pub fn start(base_url: &str, poll_interval: Duration) -> Self {
        let transitions: Arc<Mutex<Vec<(std::time::Instant, StreamStatusState)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let tx = transitions.clone();
        let url = format!("{}/streams", base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let handle = tokio::spawn(async move {
            let mut prev: Option<StreamStatusState> = None;
            loop {
                let st = async {
                    let resp = client.get(&url).send().await.ok()?;
                    let streams = resp.json::<Vec<StreamStatus>>().await.ok()?;
                    streams.first().map(|s| s.state)
                }
                .await;

                if let Some(st) = st {
                    if prev.as_ref() != Some(&st) {
                        tx.lock().unwrap().push((std::time::Instant::now(), st));
                        prev = Some(st);
                    }
                }
                tokio::time::sleep(poll_interval).await;
            }
        });
        Self {
            handle,
            transitions,
        }
    }

    pub fn stop(self) -> Vec<(std::time::Instant, StreamStatusState)> {
        self.handle.abort();
        let t = self.transitions.lock().unwrap().clone();
        t
    }

    pub fn transitions_so_far(&self) -> Vec<(std::time::Instant, StreamStatusState)> {
        self.transitions.lock().unwrap().clone()
    }
}

impl McmClient {
    pub fn new(base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(2))
            .build()
            .expect("building reqwest client");
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn list_streams(&self) -> Result<Vec<StreamStatus>> {
        let resp = self
            .client
            .get(format!("{}/streams", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        resp.json().await.context("deserializing /streams")
    }

    pub async fn create_stream(&self, post: &PostStream) -> Result<Vec<StreamStatus>> {
        self.client
            .post(format!("{}/streams", self.base_url))
            .json(post)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserializing POST /streams")
    }

    pub async fn delete_stream(&self, name: &str) -> Result<Vec<StreamStatus>> {
        self.client
            .delete(format!("{}/delete_stream", self.base_url))
            .query(&[("name", name)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("deserializing DELETE /delete_stream")
    }

    pub async fn thumbnail(&self, source: &str) -> Result<reqwest::Response> {
        Ok(self
            .client
            .get(format!("{}/thumbnail", self.base_url))
            .query(&[("source", source)])
            .send()
            .await?)
    }

    pub fn build_fake_h264_rtsp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://0.0.0.0:{rtsp_port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("H264".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: ext,
            },
        }
    }

    pub fn build_fake_h264_udp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        host: &str,
        port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("H264".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: None,
            },
        }
    }

    pub fn build_fake_h265_rtsp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://0.0.0.0:{rtsp_port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("H265".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: ext,
            },
        }
    }

    pub fn build_fake_h265_udp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        host: &str,
        port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp265://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("H265".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: None,
            },
        }
    }

    pub fn build_fake_mjpg_rtsp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://0.0.0.0:{rtsp_port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("MJPG".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: ext,
            },
        }
    }

    pub fn build_fake_mjpg_udp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        host: &str,
        port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("MJPG".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: None,
            },
        }
    }

    pub fn build_fake_yuyv_rtsp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://0.0.0.0:{rtsp_port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("YUYV".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: ext,
            },
        }
    }

    pub fn build_fake_yuyv_udp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        host: &str,
        port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "ball".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("YUYV".to_string()),
                    height,
                    width,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: None,
            },
        }
    }

    pub fn build_qr_h264_rtsp(
        name: &str,
        size: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "QRTimeStamp".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://0.0.0.0:{rtsp_port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("H264".to_string()),
                    height: size,
                    width: size,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: ext,
            },
        }
    }

    pub fn build_qr_rgb_rtsp(
        name: &str,
        size: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "QRTimeStamp".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://0.0.0.0:{rtsp_port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("RGB".to_string()),
                    height: size,
                    width: size,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: ext,
            },
        }
    }

    pub fn build_qr_h264_udp(name: &str, size: u32, fps: u32, host: &str, port: u16) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "QRTimeStamp".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("H264".to_string()),
                    height: size,
                    width: size,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: None,
            },
        }
    }

    pub fn build_qr_rgb_udp(name: &str, size: u32, fps: u32, host: &str, port: u16) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "QRTimeStamp".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String("RGB".to_string()),
                    height: size,
                    width: size,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: fps,
                    },
                }),
                extended_configuration: None,
            },
        }
    }

    pub fn build_redirect_udp(name: &str, host: &str, port: u16) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "Redirect".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("udp://{host}:{port}")).unwrap()],
                configuration: CaptureConfiguration::Redirect {},
                extended_configuration: Some(ExtendedConfiguration {
                    disable_mavlink: true,
                    disable_zenoh: true,
                    ..Default::default()
                }),
            },
        }
    }

    pub fn build_redirect_rtsp(name: &str, host: &str, port: u16, path: &str) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: "Redirect".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(&format!("rtsp://{host}:{port}/{path}")).unwrap()],
                configuration: CaptureConfiguration::Redirect {},
                extended_configuration: Some(ExtendedConfiguration {
                    disable_mavlink: true,
                    disable_zenoh: true,
                    ..Default::default()
                }),
            },
        }
    }

    pub async fn wait_for_streams_running(
        &self,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<StreamStatus>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let streams = self.list_streams().await?;
            let running = streams.iter().filter(|s| s.running).count();
            if running >= count {
                return Ok(streams);
            }
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!(
                    "only {running}/{count} streams running after {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn wait_for_stream_state(
        &self,
        expected: StreamStatusState,
        timeout: Duration,
    ) -> Result<Vec<StreamStatus>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let streams = self.list_streams().await?;
            if streams.iter().all(|s| s.state == expected) && !streams.is_empty() {
                return Ok(streams);
            }
            if tokio::time::Instant::now() > deadline {
                let states: Vec<_> = streams.iter().map(|s| &s.state).collect();
                anyhow::bail!(
                    "expected all streams {:?}, got {:?} after {}s",
                    expected,
                    states,
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Wait for a named stream to leave the Running/Waking phase and
    /// reach Draining or Idle. This confirms the stream completed its
    /// initial Waking lifecycle (pipeline created, RTSP factory mounted
    /// and preserved).
    pub async fn wait_for_stream_idle(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<StreamStatus> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let streams = self.list_streams().await?;
            if let Some(s) = streams
                .into_iter()
                .find(|s| s.video_and_stream.name == name)
            {
                if !s.running {
                    return Ok(s);
                }
            }
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!(
                    "stream {name:?} did not reach idle within {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Helper: open a signalling WebSocket, get peer ID and producer ID for
/// the first available stream, then start a session. Returns the bind
/// answer and the (sink, stream) halves of the WS connection.
///
/// Retries transient connection errors (e.g. ECONNRESET under CI load)
/// until a 15-second deadline, matching the resilience pattern used in
/// `start_webrtc_session_for_producer`.
pub async fn start_webrtc_session(
    signalling_url: &str,
) -> Result<(
    BindAnswer,
    futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last_error: Option<anyhow::Error> = None;

    loop {
        if tokio::time::Instant::now() > deadline {
            if let Some(err) = last_error {
                anyhow::bail!("start_webrtc_session timed out after 15s, last error: {err:#}");
            }
            anyhow::bail!("start_webrtc_session timed out after 15s");
        }

        match attempt_webrtc_session(signalling_url).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                last_error = Some(err);
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn attempt_webrtc_session(
    signalling_url: &str,
) -> Result<(
    BindAnswer,
    futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
)> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let (ws, _) = connect_async(signalling_url).await?;
    let (mut sink, mut stream) = ws.split();

    let ask = |q: SignallingQuestion| SignallingProtocol {
        message: SignallingMessage::Question(q),
    };

    async fn next_text(
        stream: &mut futures::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) -> Result<String> {
        loop {
            let msg = stream.next().await.context("ws closed")??;
            if let Message::Text(t) = msg {
                return Ok(t.to_string());
            }
        }
    }

    // Get peer ID
    let text = serde_json::to_string(&ask(SignallingQuestion::PeerId))?;
    sink.send(Message::Text(text.into())).await?;
    let raw = next_text(&mut stream).await?;
    let proto: SignallingProtocol = serde_json::from_str(&raw)?;
    let consumer_id = match proto.message {
        SignallingMessage::Answer(SignallingAnswer::PeerId(a)) => a.id,
        other => anyhow::bail!("expected PeerId, got {other:?}"),
    };

    // Get available streams
    let text = serde_json::to_string(&ask(SignallingQuestion::AvailableStreams))?;
    sink.send(Message::Text(text.into())).await?;
    let raw = next_text(&mut stream).await?;
    let proto: SignallingProtocol = serde_json::from_str(&raw)?;
    let producer_id = match proto.message {
        SignallingMessage::Answer(SignallingAnswer::AvailableStreams(ref s)) => {
            s.first().context("no available streams")?.id
        }
        other => anyhow::bail!("expected AvailableStreams, got {other:?}"),
    };

    // Start session
    let offer = BindOffer {
        consumer_id,
        producer_id,
    };
    let text = serde_json::to_string(&ask(SignallingQuestion::StartSession(offer)))?;
    sink.send(Message::Text(text.into())).await?;

    // Wait for StartSession answer (skip Negotiation and non-text messages)
    let bind = loop {
        let raw = next_text(&mut stream).await?;
        let proto: SignallingProtocol = serde_json::from_str(&raw)?;
        match proto.message {
            SignallingMessage::Answer(SignallingAnswer::StartSession(b)) => break b,
            SignallingMessage::Negotiation(_) => continue,
            other => anyhow::bail!("unexpected message: {other:?}"),
        }
    };

    Ok((bind, sink, stream))
}

/// Like `start_webrtc_session`, but targets a specific producer by name.
/// Polls the signalling server until the target producer appears in the
/// available streams (the redirect stream's encode needs time to resolve).
/// Returns the bind answer, available streams list, and the WS halves.
pub async fn start_webrtc_session_for_producer(
    signalling_url: &str,
    producer_name: &str,
    timeout: Duration,
) -> Result<(
    BindAnswer,
    Vec<AvailableStream>,
    futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
)> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_error: Option<anyhow::Error> = None;

    // Poll until the target producer appears in available streams.
    // Each iteration opens a fresh WebSocket to avoid stale state.
    // Transient errors (connection reset, ws closed, etc.) are retried
    // until the deadline rather than propagated immediately.
    loop {
        if tokio::time::Instant::now() > deadline {
            if let Some(err) = last_error {
                anyhow::bail!(
                    "producer {producer_name:?} not found after {}s, last error: {err:#}",
                    timeout.as_secs()
                );
            }
            anyhow::bail!(
                "producer {producer_name:?} not found in available streams after {}s",
                timeout.as_secs()
            );
        }

        let attempt = async {
            let (ws, _) = connect_async(signalling_url).await?;
            let (mut sink, mut stream) = ws.split();

            let ask = |q: SignallingQuestion| SignallingProtocol {
                message: SignallingMessage::Question(q),
            };

            // Helper: read the next Text frame, skipping non-text messages.
            async fn next_text_msg(
                stream: &mut futures::stream::SplitStream<
                    tokio_tungstenite::WebSocketStream<
                        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                    >,
                >,
            ) -> Result<String> {
                loop {
                    let msg = stream.next().await.context("ws closed")??;
                    if let Message::Text(t) = msg {
                        return Ok(t.to_string());
                    }
                }
            }

            // Get peer ID
            let text = serde_json::to_string(&ask(SignallingQuestion::PeerId))?;
            sink.send(Message::Text(text.into())).await?;
            let raw = next_text_msg(&mut stream).await?;
            let proto: SignallingProtocol = serde_json::from_str(&raw)?;
            let consumer_id = match proto.message {
                SignallingMessage::Answer(SignallingAnswer::PeerId(a)) => a.id,
                other => anyhow::bail!("expected PeerId, got {other:?}"),
            };

            // Get available streams
            let text = serde_json::to_string(&ask(SignallingQuestion::AvailableStreams))?;
            sink.send(Message::Text(text.into())).await?;
            let raw = next_text_msg(&mut stream).await?;
            let proto: SignallingProtocol = serde_json::from_str(&raw)?;
            let available = match proto.message {
                SignallingMessage::Answer(SignallingAnswer::AvailableStreams(s)) => s,
                other => anyhow::bail!("expected AvailableStreams, got {other:?}"),
            };

            if let Some(target) = available.iter().find(|s| s.name.contains(producer_name)) {
                let producer_id = target.id;

                // Start session
                let offer = BindOffer {
                    consumer_id,
                    producer_id,
                };
                let text = serde_json::to_string(&ask(SignallingQuestion::StartSession(offer)))?;
                sink.send(Message::Text(text.into())).await?;

                // Wait for StartSession answer (skip Negotiation and non-text messages)
                let bind = loop {
                    let raw = next_text_msg(&mut stream).await?;
                    let proto: SignallingProtocol = serde_json::from_str(&raw)?;
                    match proto.message {
                        SignallingMessage::Answer(SignallingAnswer::StartSession(b)) => break b,
                        SignallingMessage::Negotiation(_) => continue,
                        other => anyhow::bail!("unexpected message: {other:?}"),
                    }
                };

                return Ok(Some((bind, available, sink, stream)));
            }

            // Close this connection and retry
            drop(stream);
            let _ = sink.close().await;

            anyhow::Ok(None)
        };

        match attempt.await {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => {
                // Producer not found yet, retry after a short sleep
            }
            Err(err) => {
                last_error = Some(err);
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// End a WebRTC session via the signalling WebSocket.
pub async fn end_webrtc_session(
    sink: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    bind: &BindAnswer,
) -> Result<()> {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let end = EndSessionQuestion {
        bind: bind.clone(),
        reason: "test_done".into(),
    };
    let msg = SignallingProtocol {
        message: SignallingMessage::Question(SignallingQuestion::EndSession(end)),
    };
    let text = serde_json::to_string(&msg)?;
    sink.send(Message::Text(text.into())).await?;
    Ok(())
}

/// Compute the zenoh topic that MCM publishes for a given stream name.
/// Mirrors the server-side logic in `ZenohSink::try_new`.
pub fn zenoh_topic(stream_name: &str) -> String {
    let alphanum: String = stream_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("video/{alphanum}/stream")
}
