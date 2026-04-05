use std::time::Duration;

use stream_clients::browser_webrtc_client::BrowserWebrtcClient;

use super::*;

/// Browser-based cold-vs-warm WebRTC decode test.
///
/// Uses headless Chrome via WebDriver to connect/disconnect 5 times,
/// verifying that `RTCPeerConnection.getStats()` reports `framesDecoded > 0`
/// on every cycle -- including warm reconnections where the GStreamer pipeline
/// was never torn down.
///
/// Requires `chromedriver` and `google-chrome` (or `chromium`) in PATH.
/// Gated behind `#[cfg(feature = "webrtc-test")]` -- enable with:
///   `cargo test --features webrtc-test test_webrtc_browser_warm_decode`
/// See `.github/workflows/test_webrtc_leak.yml` for the CI setup that
/// installs Chrome/Chromedriver and enables this feature.
#[tokio::test]
async fn test_webrtc_browser_warm_decode() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let frontend_url = format!("{}/webrtc", env.rest_url);
    let browser = BrowserWebrtcClient::new()
        .await
        .expect("Failed to create browser client");

    for cycle in 0..5u32 {
        let is_cold = cycle == 0;
        let playing_timeout = if is_cold {
            cold_timeout()
        } else {
            warm_timeout()
        };
        let decode_timeout = if is_cold {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(20)
        };

        browser
            .connect(&frontend_url, env.signalling_port)
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle}: connect failed: {e}"));

        browser
            .wait_for_playing(playing_timeout)
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle}: never reached Playing: {e}"));

        let stats = browser
            .wait_for_decoded_frames(5, decode_timeout)
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle}: {e}"));

        eprintln!(
            "cycle {cycle}: decoded={} received={} dropped={} keyFrames={}",
            stats.frames_decoded,
            stats.frames_received,
            stats.frames_dropped,
            stats.key_frames_decoded,
        );

        browser
            .disconnect()
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle}: disconnect failed: {e}"));

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    browser.quit().await.ok();
}
