use std::time::Duration;

use stream_clients::FrameSample;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    let parsed: url::Url = url.parse().expect("rtsp url");
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(8554);
    let addr = format!("{host}:{port}");
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_options = String::from("no response");
    loop {
        let factory_ready = async {
            let mut stream = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            .ok()?
            .ok()?;
            let request = format!("OPTIONS rtsp://{addr}{path} RTSP/1.0\r\nCSeq: 1\r\n\r\n");
            stream.write_all(request.as_bytes()).await.ok()?;
            let mut buffer = [0u8; 256];
            let bytes_read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
                .await
                .ok()?
                .ok()?;
            let response = std::str::from_utf8(&buffer[..bytes_read]).unwrap_or("");
            Some(response.lines().next().unwrap_or(response).to_string())
        }
        .await;
        match factory_ready {
            Some(status) if status.starts_with("RTSP/1.0 200") => return,
            Some(status) => last_options = status,
            None => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "RTSP factory at {addr}{path} not serving within {timeout:?} (last OPTIONS: {last_options})"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
