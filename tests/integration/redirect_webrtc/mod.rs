mod thumbnail;
mod webrtc;
mod zenoh;

use std::time::Duration;

pub(super) use stream_clients::{Codec, FrameSample};
pub(super) use tokio::sync::mpsc;

pub(super) use crate::common::{
    api::{end_webrtc_session, start_webrtc_session_for_producer, zenoh_topic, McmClient},
    mcm::{allocate_udp_ports, McmProcess},
    types::*,
};

pub(super) const TIMEOUT: Duration = Duration::from_secs(60);

/// Spawn an external gst-launch-1.0 process that sends H264 RTP packets
/// to the given host:port. This avoids MCM endpoint conflicts and ensures
/// packets arrive on the correct loopback interface.
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

pub(super) fn spawn_h265_udp_sender(host: &str, port: u16) -> std::process::Child {
    std::process::Command::new("gst-launch-1.0")
        .args([
            "videotestsrc",
            "is-live=true",
            "pattern=ball",
            "do-timestamp=true",
            "!",
            "video/x-raw,width=160,height=120,framerate=30/1,format=I420",
            "!",
            "x265enc",
            "tune=zerolatency",
            "speed-preset=ultrafast",
            "bitrate=5000",
            "!",
            "h265parse",
            "config-interval=-1",
            "!",
            "video/x-h265,stream-format=byte-stream,alignment=au",
            "!",
            "rtph265pay",
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
        .expect("failed to start gst-launch-1.0 H265 UDP sender")
}

/// Start MCM with a lazy redirect receiver on the given port, and an
/// external GStreamer sender providing H264 RTP to that port. The redirect
/// is lazy -- the test functions trigger the wake-up chain themselves.
pub(super) async fn setup_udp_redirect() -> (McmProcess, McmClient, std::process::Child) {
    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_h264_udp_sender("127.0.0.1", udp_port);

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
    let mut sender = spawn_h265_udp_sender("127.0.0.1", udp_port);

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

    let redirect =
        McmClient::build_redirect_rtsp("redirect_receiver", "127.0.0.1", mcm.rtsp_port, path);
    client.create_stream(&redirect).await.unwrap();

    client
        .wait_for_stream_idle("redirect_receiver", TIMEOUT)
        .await
        .expect("redirect should complete initial lifecycle");

    (mcm, client)
}

/// Poll `GET /thumbnail?source=...` until it returns 200 with a non-empty
/// body, or time out.
pub(super) async fn wait_for_thumbnail(
    client: &McmClient,
    source: &str,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(resp) = client.thumbnail(source).await {
            if resp.status().is_success() {
                let bytes = resp.bytes().await.unwrap_or_default();
                if !bytes.is_empty() {
                    return bytes.to_vec();
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "thumbnail for {source:?} not available after {}s",
            timeout.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(super) fn drain_zenoh(rx: &mut mpsc::UnboundedReceiver<FrameSample>) -> Vec<FrameSample> {
    let mut samples = Vec::new();
    while let Ok(s) = rx.try_recv() {
        samples.push(s);
    }
    samples
}

pub(super) async fn wait_first_zenoh_frame(
    rx: &mut mpsc::UnboundedReceiver<FrameSample>,
    timeout: Duration,
    label: &str,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !drain_zenoh(rx).is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "No {label} frames received within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
