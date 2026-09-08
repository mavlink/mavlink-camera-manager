use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use stream_clients::{
    Codec,
    protocol::{
        self, Answer, BindAnswer, BindOffer, EndSessionQuestion, Protocol, Question, Stream,
    },
};
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

    fn build_video_stream(
        name: &str,
        source: &str,
        encode: &str,
        endpoint: &str,
        width: u32,
        height: u32,
        fps: u32,
        ext: Option<ExtendedConfiguration>,
    ) -> PostStream {
        PostStream {
            name: name.to_string(),
            source: source.to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse(endpoint).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: serde_json::Value::String(encode.to_string()),
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

    fn video_encode(codec: Codec) -> &'static str {
        match codec {
            Codec::H264 => "H264",
            Codec::H265 => "H265",
            Codec::Mjpg => "MJPG",
            Codec::Yuyv => "YUYV",
            Codec::Rgb => "RGB",
        }
    }

    pub fn build_fake_rtsp(
        codec: Codec,
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        if codec == Codec::Rgb {
            panic!("Fake pipeline does not support RGB; use QR pipeline instead");
        }
        Self::build_video_stream(
            name,
            "ball",
            Self::video_encode(codec),
            &format!("rtsp://0.0.0.0:{rtsp_port}/{path}"),
            width,
            height,
            fps,
            ext,
        )
    }

    pub fn build_fake_udp(
        codec: Codec,
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        host: &str,
        port: u16,
        ext: Option<ExtendedConfiguration>,
    ) -> PostStream {
        let scheme = match codec {
            Codec::H265 => "udp265",
            _ => "udp",
        };
        Self::build_video_stream(
            name,
            "ball",
            Self::video_encode(codec),
            &format!("{scheme}://{host}:{port}"),
            width,
            height,
            fps,
            ext,
        )
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
        Self::build_fake_rtsp(Codec::H264, name, width, height, fps, path, ext, rtsp_port)
    }

    pub fn build_fake_h264_udp(
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        host: &str,
        port: u16,
    ) -> PostStream {
        Self::build_fake_udp(Codec::H264, name, width, height, fps, host, port, None)
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
        Self::build_fake_rtsp(Codec::H265, name, width, height, fps, path, ext, rtsp_port)
    }

    pub fn build_qr_h264_rtsp(
        name: &str,
        size: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        Self::build_video_stream(
            name,
            "QRTimeStamp",
            "H264",
            &format!("rtsp://0.0.0.0:{rtsp_port}/{path}"),
            size,
            size,
            fps,
            ext,
        )
    }

    pub fn build_qr_rgb_rtsp(
        name: &str,
        size: u32,
        fps: u32,
        path: &str,
        ext: Option<ExtendedConfiguration>,
        rtsp_port: u16,
    ) -> PostStream {
        Self::build_video_stream(
            name,
            "QRTimeStamp",
            "RGB",
            &format!("rtsp://0.0.0.0:{rtsp_port}/{path}"),
            size,
            size,
            fps,
            ext,
        )
    }

    pub fn build_qr_h264_udp(name: &str, size: u32, fps: u32, host: &str, port: u16) -> PostStream {
        Self::build_video_stream(
            name,
            "QRTimeStamp",
            "H264",
            &format!("udp://{host}:{port}"),
            size,
            size,
            fps,
            None,
        )
    }

    pub fn build_qr_rgb_udp(name: &str, size: u32, fps: u32, host: &str, port: u16) -> PostStream {
        Self::build_video_stream(
            name,
            "QRTimeStamp",
            "RGB",
            &format!("udp://{host}:{port}"),
            size,
            size,
            fps,
            None,
        )
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
    let (bind, _available, sink, stream) =
        webrtc_signalling_handshake(signalling_url, None, Duration::from_secs(15)).await?;
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
    Vec<Stream>,
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
    webrtc_signalling_handshake(signalling_url, Some(producer_name), timeout).await
}

async fn webrtc_signalling_handshake(
    signalling_url: &str,
    producer_name: Option<&str>,
    timeout: Duration,
) -> Result<(
    BindAnswer,
    Vec<Stream>,
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

    loop {
        if tokio::time::Instant::now() > deadline {
            if let Some(name) = producer_name {
                if let Some(err) = last_error {
                    anyhow::bail!(
                        "producer {name:?} not found after {}s, last error: {err:#}",
                        timeout.as_secs()
                    );
                }
                anyhow::bail!(
                    "producer {name:?} not found in available streams after {}s",
                    timeout.as_secs()
                );
            }
            if let Some(err) = last_error {
                anyhow::bail!(
                    "start_webrtc_session timed out after {}s, last error: {err:#}",
                    timeout.as_secs()
                );
            }
            anyhow::bail!(
                "start_webrtc_session timed out after {}s",
                timeout.as_secs()
            );
        }

        let attempt = async {
            let (ws, _) = connect_async(signalling_url).await?;
            let (mut sink, mut stream) = ws.split();

            let ask = |q: Question| Protocol {
                message: protocol::Message::Question(q),
            };

            let text = serde_json::to_string(&ask(Question::PeerId))?;
            sink.send(Message::Text(text.into())).await?;
            let raw = next_signalling_text(&mut stream).await?;
            let proto: Protocol = serde_json::from_str(&raw)?;
            let consumer_id = match proto.message {
                protocol::Message::Answer(Answer::PeerId(a)) => a.id,
                other => anyhow::bail!("expected PeerId, got {other:?}"),
            };

            let text = serde_json::to_string(&ask(Question::AvailableStreams))?;
            sink.send(Message::Text(text.into())).await?;
            let raw = next_signalling_text(&mut stream).await?;
            let proto: Protocol = serde_json::from_str(&raw)?;
            let available = match proto.message {
                protocol::Message::Answer(Answer::AvailableStreams(s)) => s,
                other => anyhow::bail!("expected AvailableStreams, got {other:?}"),
            };

            let producer_id = match producer_name {
                Some(name) => available
                    .iter()
                    .find(|s| s.name.contains(name))
                    .map(|s| s.id),
                None => available.first().map(|s| s.id),
            };

            let Some(producer_id) = producer_id else {
                drop(stream);
                let _ = sink.close().await;
                return anyhow::Ok(None);
            };

            let offer = BindOffer {
                consumer_id,
                producer_id,
            };
            let text = serde_json::to_string(&ask(Question::StartSession(offer)))?;
            sink.send(Message::Text(text.into())).await?;

            let bind = loop {
                let raw = next_signalling_text(&mut stream).await?;
                let proto: Protocol = serde_json::from_str(&raw)?;
                match proto.message {
                    protocol::Message::Answer(Answer::StartSession(b)) => break b,
                    protocol::Message::Negotiation(_) => continue,
                    other => anyhow::bail!("unexpected message: {other:?}"),
                }
            };

            Ok(Some((bind, available, sink, stream)))
        };

        match attempt.await {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => {}
            Err(err) => {
                last_error = Some(err);
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn next_signalling_text(
    stream: &mut futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) -> Result<String> {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        let msg = stream.next().await.context("ws closed")??;
        if let Message::Text(t) = msg {
            return Ok(t.to_string());
        }
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
    let msg = Protocol {
        message: protocol::Message::Question(Question::EndSession(end)),
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
