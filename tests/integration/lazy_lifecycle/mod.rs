mod mixed;
mod recovery;
mod rtsp;
mod state_transitions;
mod thumbnail;
mod webrtc;

use std::time::Duration;

pub(super) use stream_clients::{Codec, StreamClient, rtsp_client::RtspClient};
pub(super) use tokio::sync::mpsc;

pub(super) use crate::common::{
    api::{McmClient, StateMonitor, end_webrtc_session, start_webrtc_session, zenoh_topic},
    gst_sender::spawn_udp_sender,
    mcm::{McmProcess, allocate_udp_ports},
    poll::drain,
    types::*,
};

pub(super) const TIMEOUT: Duration = Duration::from_secs(15);

/// The watcher's idle grace period before suspending.
pub(super) const IDLE_GRACE: Duration = Duration::from_secs(5);

/// Extra slack so the watcher loop (100 ms tick) has time to observe
/// the idle condition and flip the state.
pub(super) const IDLE_WAIT: Duration = Duration::from_secs(8);

pub(super) async fn setup_fake_rtsp(name: &str, path: &str) -> (McmProcess, McmClient) {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let post = McmClient::build_fake_h264_rtsp(name, 640, 480, 30, path, None, mcm.rtsp_port);
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    (mcm, client)
}

pub(super) async fn wait_for_idle(client: &McmClient) {
    tokio::time::sleep(IDLE_WAIT).await;
    client
        .wait_for_stream_state(StreamStatusState::Idle, TIMEOUT)
        .await
        .unwrap();
}

/// Connect an RTSP client using raw TCP and verify the server responds.
/// Returns true if we got a valid RTSP response to OPTIONS.
pub(super) async fn rtsp_options_ok(rtsp_url: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let url: url::Url = rtsp_url.parse().unwrap();
    let host = url.host_str().unwrap_or("127.0.0.1");
    let port = url.port().unwrap_or(8554);
    let addr = format!("{host}:{port}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match tokio::net::TcpStream::connect(&addr).await {
            Ok(s) => break s,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(_) => return false,
        }
    };

    let request = format!("OPTIONS {rtsp_url} RTSP/1.0\r\nCSeq: 1\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }

    let mut buf = vec![0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            let resp = String::from_utf8_lossy(&buf[..n]);
            resp.contains("RTSP/1.0")
        }
        _ => false,
    }
}
