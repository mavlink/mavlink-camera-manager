use std::time::Duration;

use stream_clients::FrameSample;
use tokio::sync::mpsc;

use super::api::McmClient;

/// Drain all pending samples from the channel and return them.
pub fn drain(rx: &mut mpsc::UnboundedReceiver<FrameSample>) -> Vec<FrameSample> {
    let mut samples = Vec::new();
    while let Ok(s) = rx.try_recv() {
        samples.push(s);
    }
    samples
}

/// Wait for the first frame to arrive on the channel, or panic after timeout.
pub async fn wait_first_frame(
    rx: &mut mpsc::UnboundedReceiver<FrameSample>,
    timeout: Duration,
    label: &str,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !drain(rx).is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "No {label} frames received within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Poll `GET /thumbnail?source=...` until it returns 200 with a non-empty
/// body, or time out.
pub async fn wait_for_thumbnail(client: &McmClient, source: &str, timeout: Duration) -> Vec<u8> {
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

pub async fn wait_for_rtsp_tcp(url: &str, timeout: Duration) {
    let addr = url
        .trim_start_matches("rtsp://")
        .split('/')
        .next()
        .unwrap_or("127.0.0.1:8554");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "RTSP server at {addr} not accepting TCP within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
