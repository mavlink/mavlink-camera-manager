use std::net::SocketAddr;

use anyhow::{anyhow, Context, Result};
use async_tungstenite::{tokio::TokioAdapter, tungstenite, WebSocketStream};
use futures::{SinkExt, StreamExt};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tracing::*;

use crate::{cli, stream};

use super::signalling_protocol::{self, *};

#[derive(Debug)]
pub struct SignallingServer {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for SignallingServer {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        debug!("Dropping SignallingServer...");

        if let Some(handle) = self.handle.take() {
            if !handle.is_finished() {
                handle.abort();
                tokio::spawn(async move {
                    let _ = handle.await;
                    debug!("SignallingServer task aborted");
                });
            } else {
                debug!("SignallingServer task nicely finished!");
            }
        }

        debug!("SignallingServer Dropped!");
    }
}

impl Default for SignallingServer {
    #[instrument(level = "debug", fields(endpoint))]
    fn default() -> Self {
        let endpoint = url::Url::parse(cli::manager::signalling_server_address().as_str())
            .expect("Wrong default signalling endpoint");

        debug!("Starting SignallingServer task...");

        let handle = Some(tokio::spawn(async move {
            debug!("SignallingServer task started!");
            match SignallingServer::runner(endpoint).await {
                Ok(()) => debug!("SignallingServer task eneded with no errors"),
                Err(error) => warn!("SignallingServer task ended with error: {error:#?}"),
            }
        }));

        Self { handle }
    }
}

impl SignallingServer {
    #[instrument(level = "debug")]
    async fn runner(endpoint: url::Url) -> Result<()> {
        let host = endpoint
            .host()
            .context(format!("Failed to get the host from {endpoint:#?}"))?;
        let port = endpoint
            .port()
            .context(format!("Failed to get the port from {endpoint:#?}"))?;

        let addr = format!("{host}:{port}").parse::<SocketAddr>()?;

        // Create the event loop and TCP listener we'll accept connections on.
        let listener = TcpListener::bind(&addr).await?;
        debug!("Signalling server: listening on: {addr:?}");

        while let Ok((stream, address)) = listener.accept().await {
            info!("Accepting connection from {address:?}");

            tokio::spawn(Self::accept_connection(stream));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(stream))]
    async fn accept_connection(stream: TcpStream) {
        debug!("Accepting connection...");

        let stream = match async_tungstenite::tokio::accept_async(stream).await {
            Ok(stream) => stream,
            Err(error) => {
                error!("Failed to accept websocket connection: {error:?}");
                return;
            }
        };

        if let Err(error) = Self::handle_connection(stream).await {
            error!("Error processing connection: {error}");
        }
    }

    #[instrument(level = "debug", skip(stream))]
    async fn handle_connection(stream: WebSocketStream<TokioAdapter<TcpStream>>) -> Result<()> {
        info!("New Signalling connection");

        let (mut ws_sink, mut ws_stream) = stream.split();

        // This MPSC channel is used to transmit messages to websocket from Session
        let (mpsc_sender, mut mpsc_receiver) = mpsc::unbounded_channel::<Result<Message>>();

        // Create a sender task, which receives from the mpsc channel
        let sender_task_handle = tokio::spawn(async move {
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(30), mpsc_receiver.recv())
                    .await
                {
                    Ok(Some(Ok(message))) => {
                        if let Message::Question(Question::EndSession(end_session_question)) =
                            message
                        {
                            let bind = end_session_question.bind;
                            let reason = end_session_question.reason;

                            if let Err(error) = stream::Manager::remove_session(&bind, reason).await
                            {
                                error!("Failed removing session: {bind:?}. Reason: {error}",);
                            }

                            info!("Session: {bind:?} ended by consumer");
                            continue;
                        }

                        let message = serde_json::to_string(&message)?;
                        ws_sink.send(tungstenite::Message::Text(message)).await?
                    }
                    reason @ (Ok(Some(Err(_))) | Ok(None)) => {
                        if let Err(error) = ws_sink.send(tungstenite::Message::Close(None)).await {
                            warn!("Failed sending Close message: {error}");
                        }

                        if let Err(error) = ws_sink.close().await {
                            warn!("Failed closing the WebSocket: {error:#?}");
                        }

                        info!("WebSocket connection closed: {reason:?}");

                        break;
                    }
                    Err(_elapsed) => ws_sink.send(tungstenite::Message::Ping(vec![])).await?,
                };
            }

            anyhow::Ok(())
        });

        let receiver_task_handle = tokio::spawn(async move {
            while let Some(msg) = ws_stream.next().await {
                let msg = match msg {
                    Ok(tungstenite::Message::Text(msg)) => msg,
                    Ok(tungstenite::Message::Close(close)) => {
                        debug!("Websocket closed by client: {close:?}");
                        break;
                    }
                    Ok(tungstenite::Message::Pong(_)) => continue,
                    msg @ Ok(_) => {
                        warn!("Unsupported message type: {msg:?}");
                        continue;
                    }
                    Err(error) => {
                        error!("Failed receiving message from WebSocket: {error:?}.");
                        break;
                    }
                };

                if let Err(error) = Self::handle_message(msg.clone(), &mpsc_sender).await {
                    error!("Failed handling message: {error}");
                    break;
                }
            }

            debug!("Finishing Signalling connection...");

            anyhow::Ok(())
        });

        let _ = tokio::join!(sender_task_handle, receiver_task_handle);

        debug!("Signalling connection terminated");

        Ok(())
    }

    #[instrument(level = "debug", skip(sender))]
    async fn handle_message(
        msg: String,
        sender: &mpsc::UnboundedSender<Result<Message>>,
    ) -> Result<()> {
        let protocol = match serde_json::from_str::<Protocol>(&msg) {
            Ok(protocol) => protocol,
            Err(error) => {
                // Parsing errors should not be propagated, otherwise it will close the WebSocket.
                warn!("Ignoring received message {msg:?}. Reason: {error:#?}");
                return Ok(());
            }
        };

        trace!("Received: {protocol:#?}");
        let answer = match protocol.message {
            Message::Question(question) => {
                match question {
                    Question::PeerId => Some(Answer::PeerId(PeerIdAnswer {
                        id: stream::Manager::generate_uuid(),
                    })),
                    Question::AvailableStreams => {
                        // This looks something dumb, but in fact, by keeping signalling_protocol::Stream and
                        // webrtc_manager::VideoAndStreamInformation as different things, we can change internal logics
                        // without changing the protocol's interface.
                        let streams = Self::streams_information().await.unwrap_or_default();
                        Some(Answer::AvailableStreams(streams))
                    }
                    Question::StartSession(bind) => {
                        // After this point, any further negotiation will be sent from webrtcbin,
                        // which will use this mpsc channel's sender to queue the message for the
                        // WebSocket, which will receive and send it to the consumer via WebSocket.
                        stream::Manager::add_session(&bind, sender.clone())
                            .await
                            .context("Failed adding session.")?;

                        None
                    }
                    Question::EndSession(end_session_question) => {
                        let bind = end_session_question.bind;
                        let reason = end_session_question.reason;

                        if let Err(error) = stream::Manager::remove_session(&bind, reason).await {
                            error!("Failed removing session {bind:?}. Reason: {error}",);
                        }
                        return Err(anyhow!("Session {bind:?} ended by consumer"));
                    }
                }
            }
            Message::Answer(answer) => {
                return Err(anyhow!("Ignoring message {answer:#?}"));
            }
            Message::Negotiation(negotiation) => match negotiation {
                Negotiation::MediaNegotiation(negotiation) => {
                    let bind = negotiation.bind;
                    let sdp = negotiation.sdp;

                    stream::Manager::handle_sdp(&bind, &sdp)
                        .await
                        .context("Failed handling SDP")?;

                    None
                }
                Negotiation::IceNegotiation(negotiation) => {
                    let bind = negotiation.bind;
                    let candidate = negotiation.ice.candidate.context("No candidate -> Done")?;
                    let sdp_m_line_index = negotiation
                        .ice
                        .sdp_m_line_index
                        .context("Missing sdp_m_line_index")?;

                    stream::Manager::handle_ice(&bind, sdp_m_line_index, &candidate)
                        .await
                        .context("Failed handling ICE")?;

                    None
                }
            },
        };

        if let Some(answer) = answer {
            if let Err(reason) = sender.send(Ok(Message::from(answer))) {
                return Err(anyhow!(
                    "Failed sending message to mpsc channel. Reason: {reason:}"
                ));
            }
        }

        Ok(())
    }

    pub async fn streams_information() -> Result<Vec<Stream>> {
        let streams = stream::Manager::streams_information().await?;

        Ok(streams
            .iter()
            .filter_map(|stream| {
                let (height, width, encode, interval) =
                    match &stream.video_and_stream.stream_information.configuration {
                        crate::stream::types::CaptureConfiguration::Video(configuration) => {
                            // Filter out non-H264/h265 local streams
                            if !matches!(configuration.encode, crate::video::types::VideoEncodeType::H264 | crate::video::types::VideoEncodeType::H265) {
                                trace!("Stream {:?} will not be listed in available streams because it's encoding isn't H264 or H265 (it's {:?} instead)", stream.video_and_stream.name, configuration.encode);
                                return None;
                            }
                            (
                                Some(configuration.height),
                                Some(configuration.width),
                                Some(format!("{:#?}", configuration.encode)),
                                Some(
                                    (configuration.frame_interval.numerator as f32
                                        / configuration.frame_interval.denominator as f32)
                                        .to_string(),
                                ),
                            )
                        }
                        crate::stream::types::CaptureConfiguration::Redirect(_) => {
                            // Filter out non RTSP redirect streams
                            let scheme = stream.video_and_stream.stream_information.endpoints.first()?.scheme();
                            if scheme != "rtsp" {
                                trace!("Stream {:?} will not be listed in available streams because it's scheme isn't RTSP (it's {scheme:?} instead)", stream.video_and_stream.name);
                                return None;
                            }

                            (None, None, None, None)
                        }
                    };

                let source = Some(
                    stream
                        .video_and_stream
                        .video_source
                        .inner()
                        .source_string()
                        .to_string(),
                );

                let name = stream.video_and_stream.name.clone();
                let id = stream.id;

                Some(Stream {
                    id,
                    name,
                    encode,
                    height,
                    width,
                    interval,
                    source,
                    created: None,
                })
            })
            .collect())
    }
}

impl TryFrom<tungstenite::Message> for signalling_protocol::Protocol {
    type Error = anyhow::Error;

    #[instrument(level = "trace")]
    fn try_from(value: tungstenite::Message) -> Result<Self, Self::Error> {
        let msg = value.to_text()?;

        let protocol = serde_json::from_str::<signalling_protocol::Protocol>(msg)?;

        Ok(protocol)
    }
}

impl TryInto<tungstenite::Message> for signalling_protocol::Protocol {
    type Error = anyhow::Error;

    #[instrument(level = "trace", skip(self))]
    fn try_into(self) -> Result<tungstenite::Message, Self::Error> {
        let json_str = serde_json::to_string(&self)?;

        let msg = tungstenite::Message::Text(json_str);

        Ok(msg)
    }
}
