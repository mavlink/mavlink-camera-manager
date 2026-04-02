mod mixed;
mod recovery;
mod rtsp;
mod state_transitions;
mod thumbnail;
mod webrtc;

use std::time::Duration;

pub(super) use stream_clients::{rtsp_client::RtspClient, Codec, FrameSample, StreamClient};
pub(super) use tokio::sync::mpsc;

pub(super) use crate::common::{
    api::{end_webrtc_session, start_webrtc_session, zenoh_topic, McmClient, StateMonitor},
    mcm::{allocate_udp_ports, McmProcess},
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

pub(super) fn spawn_h264_udp_sender(host: &str, port: u16) -> std::process::Child {
    std::process::Command::new("gst-launch-1.0")
        .args([
            "videotestsrc",
            "is-live=true",
            "pattern=ball",
            "do-timestamp=true",
            "!",
            "video/x-raw,width=160,height=120,framerate=30/1",
            "!",
            "x264enc",
            "tune=zerolatency",
            "speed-preset=ultrafast",
            "bitrate=5000",
            "!",
            "h264parse",
            "config-interval=-1",
            "!",
            "video/x-h264,stream-format=avc,alignment=au",
            "!",
            "rtph264pay",
            "aggregate-mode=zero-latency",
            "config-interval=-1",
            "pt=96",
            "!",
            "udpsink",
            &format!("host={host}"),
            &format!("port={port}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start gst-launch-1.0 H264 UDP sender")
}

pub(super) fn drain_zenoh(rx: &mut mpsc::UnboundedReceiver<FrameSample>) -> Vec<FrameSample> {
    let mut samples = Vec::new();
    while let Ok(s) = rx.try_recv() {
        samples.push(s);
    }
    samples
}
