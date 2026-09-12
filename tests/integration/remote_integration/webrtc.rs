use std::time::Duration;

use super::*;

/// Cold WebRTC: pipeline goes from Idle -> Running, client receives frames.
/// Stream must NOT go through Stopped.
#[tokio::test]
async fn test_webrtc_cold_no_stopped() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let client = webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await;
    scenario_cold_no_stopped(&c, mon, &client, "WebRTC").await;
}

/// Warm WebRTC: pipeline already running, client receives frames.
#[tokio::test]
async fn test_webrtc_warm_receives_frames() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let tc = env.thumbnail_client();
    ensure_data_flowing(&tc, &env.stream_source).await;

    let client = webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await;
    scenario_warm_receives_frames(&client, Duration::from_secs(20), "WebRTC").await;
}

/// Disconnect and immediately reconnect WebRTC (user pattern 5.b).
/// Must receive frames on reconnect without errors.
#[tokio::test]
async fn test_webrtc_immediate_reconnect() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let signalling_url = env.signalling_url.clone();
    let ice_filter = env.ice_filter.clone();
    scenario_immediate_reconnect(
        &c,
        mon,
        || {
            let url = signalling_url.clone();
            let filter = ice_filter.clone();
            async move { webrtc_connect_with_retry(&url, filter.as_deref()).await }
        },
        Duration::from_secs(5),
        "WebRTC",
    )
    .await;
}

/// Wait for Idle, then reconnect (user pattern 5.a).
/// Must eventually receive frames. Stream must not go Stopped.
#[tokio::test]
async fn test_webrtc_reconnect_after_idle() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let signalling_url = env.signalling_url.clone();
    let ice_filter = env.ice_filter.clone();
    scenario_reconnect_after_idle(
        &c,
        &env.rest_url,
        || {
            let url = signalling_url.clone();
            let filter = ice_filter.clone();
            async move { webrtc_connect_with_retry(&url, filter.as_deref()).await }
        },
        "WebRTC",
    )
    .await;
}

/// Rapid WebRTC connect/disconnect cycles (3x from Idle).
/// Every cycle must deliver frames. Never go Stopped.
#[tokio::test]
async fn test_webrtc_rapid_cycles() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let mon = env.monitor();

    let signalling_url = env.signalling_url.clone();
    let ice_filter = env.ice_filter.clone();
    scenario_rapid_cycles(
        &c,
        mon,
        || {
            let url = signalling_url.clone();
            let filter = ice_filter.clone();
            async move { webrtc_connect_with_retry(&url, filter.as_deref()).await }
        },
        3,
        "WebRTC",
    )
    .await;
}

/// WebRTC returns to Idle after client disconnects.
#[tokio::test]
async fn test_webrtc_returns_to_idle() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let signalling_url = env.signalling_url.clone();
    let ice_filter = env.ice_filter.clone();
    scenario_returns_to_idle(
        &c,
        || {
            let url = signalling_url.clone();
            let filter = ice_filter.clone();
            async move { webrtc_connect_with_retry(&url, filter.as_deref()).await }
        },
        "WebRTC",
    )
    .await;
}

/// Extended cycle test: connect/settle/disconnect 5 times from Idle.
/// Each connection must receive frames. Never go Stopped.
#[tokio::test]
async fn test_webrtc_extended_cycles() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let mon = env.monitor();

    let signalling_url = env.signalling_url.clone();
    let ice_filter = env.ice_filter.clone();
    scenario_extended_cycles(
        &c,
        mon,
        || {
            let url = signalling_url.clone();
            let filter = ice_filter.clone();
            async move { webrtc_connect_with_retry(&url, filter.as_deref()).await }
        },
        5,
        "WebRTC",
    )
    .await;
}

/// Multi-warm sequential test: RTSP keeps the pipeline running while multiple
/// WebRTC clients connect/disconnect sequentially. This tests the "3rd+ always fail"
/// pattern where the pipeline never goes to Idle between WebRTC connections.
#[tokio::test]
async fn test_webrtc_multi_warm_sequential() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let anchor = RtspClient::new(&env.mcm_rtsp_url, Codec::H264, None)
        .await
        .expect("RTSP anchor");
    anchor
        .wait_for_frames(5, cold_timeout())
        .await
        .expect("RTSP anchor must receive frames");
    eprintln!("RTSP anchor established");

    let signalling_url = env.signalling_url.clone();
    let ice_filter = env.ice_filter.clone();
    scenario_multi_warm_sequential(
        &anchor,
        || {
            let url = signalling_url.clone();
            let filter = ice_filter.clone();
            async move { webrtc_connect_with_retry(&url, filter.as_deref()).await }
        },
        5,
        "RTSP anchor",
        "WebRTC",
    )
    .await;
}
