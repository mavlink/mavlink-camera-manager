mod h264_profile;
mod h265_profile;
mod thumbnail;
mod webrtc;
mod zenoh;

use std::time::Duration;

pub(super) use stream_clients::Codec;
pub(super) use tokio::sync::mpsc;

pub(super) use crate::common::{
    api::{McmClient, end_webrtc_session, start_webrtc_session_for_producer, zenoh_topic},
    gst_sender::spawn_udp_sender,
    mcm::{McmProcess, allocate_udp_ports},
    poll::{drain, wait_first_frame, wait_for_thumbnail},
    types::*,
};

pub(super) const TIMEOUT: Duration = Duration::from_secs(60);

/// Start MCM with a lazy redirect receiver on the given port, and an
/// external GStreamer sender providing H264 RTP to that port. The redirect
/// is lazy -- the test functions trigger the wake-up chain themselves.
pub(super) async fn setup_udp_redirect() -> (McmProcess, McmClient, std::process::Child) {
    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_udp_sender(Codec::H264, None, "127.0.0.1", udp_port);

    // Give the sender time to start producing frames
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let redirect = McmClient::build_redirect_udp("redirect_receiver", "127.0.0.1", udp_port);
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create redirect stream: {e}");
    });

    (mcm, client, sender)
}

/// Start MCM with a lazy Fake H264 RTSP stream and a lazy Redirect RTSP
/// receiver pointing at the same RTSP path. The fake sender is allowed to
/// go idle first (confirming its RTSP factory is mounted and preserved),
/// then the redirect is created. Neither stream is kept running -- the
/// test functions trigger the wake-up chain themselves.
pub(super) async fn setup_fake_rtsp_and_redirect(path: &str) -> (McmProcess, McmClient) {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let fake = McmClient::build_fake_h264_rtsp(
        "fake_rtsp_sender",
        160,
        120,
        30,
        path,
        None,
        mcm.rtsp_port,
    );
    client.create_stream(&fake).await.unwrap();

    client
        .wait_for_stream_idle("fake_rtsp_sender", TIMEOUT)
        .await
        .expect("fake RTSP sender should complete initial lifecycle");
    mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

    let redirect =
        McmClient::build_redirect_rtsp("redirect_receiver", "127.0.0.1", mcm.rtsp_port, path);
    client.create_stream(&redirect).await.unwrap();

    client
        .wait_for_stream_idle("redirect_receiver", TIMEOUT)
        .await
        .expect("redirect should complete initial lifecycle");

    (mcm, client)
}

pub(super) async fn setup_h265_udp_redirect() -> (McmProcess, McmClient, std::process::Child) {
    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_udp_sender(Codec::H265, None, "127.0.0.1", udp_port);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let redirect = McmClient::build_redirect_udp("redirect_receiver", "127.0.0.1", udp_port);
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create redirect stream: {e}");
    });

    (mcm, client, sender)
}

pub(super) async fn setup_fake_h265_rtsp_and_redirect(path: &str) -> (McmProcess, McmClient) {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let fake = McmClient::build_fake_h265_rtsp(
        "fake_rtsp_sender",
        160,
        120,
        30,
        path,
        None,
        mcm.rtsp_port,
    );
    client.create_stream(&fake).await.unwrap();

    client
        .wait_for_stream_idle("fake_rtsp_sender", TIMEOUT)
        .await
        .expect("fake H265 RTSP sender should complete initial lifecycle");
    mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

    let redirect =
        McmClient::build_redirect_rtsp("redirect_receiver", "127.0.0.1", mcm.rtsp_port, path);
    client.create_stream(&redirect).await.unwrap();

    client
        .wait_for_stream_idle("redirect_receiver", TIMEOUT)
        .await
        .expect("redirect should complete initial lifecycle");

    (mcm, client)
}

/// Wait for the server's `Negotiation` message on `ws_stream` and return
/// the offer SDP text. Panics if no offer is received within [`TIMEOUT`].
/// Shared by the profile-preservation tests, which all need the same
/// extraction after `start_webrtc_session_for_producer`.
pub(super) async fn read_offer_sdp_text(
    ws_stream: &mut futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) -> String {
    use futures::StreamExt;
    use stream_clients::protocol::{Message, Negotiation, Protocol, RTCSessionDescription};

    let sdp_offer = tokio::time::timeout(TIMEOUT, async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            let text = match msg.into_text() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let proto: Result<Protocol, _> = serde_json::from_str(&text);
            let Ok(proto) = proto else { continue };
            if let Message::Negotiation(Negotiation::MediaNegotiation(media)) = proto.message {
                if let RTCSessionDescription::Offer(sdp) = media.sdp {
                    return Some(sdp.sdp);
                }
            }
        }
        None
    })
    .await;

    sdp_offer
        .expect("should receive negotiation message")
        .expect("ws stream should not close before SDP offer")
}
