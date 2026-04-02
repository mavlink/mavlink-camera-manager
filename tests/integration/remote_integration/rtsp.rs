use std::time::Duration;

use super::*;

/// Cold RTSP: connects and receives frames. No Stopped state.
#[tokio::test]
async fn test_rtsp_cold_no_stopped() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let client = RtspClient::new(&env.mcm_rtsp_url, Codec::H264, None)
        .await
        .expect("rtsp client");
    scenario_cold_no_stopped(&c, mon, &client, "RTSP").await;
}

/// Warm RTSP: pipeline already running, client receives frames.
#[tokio::test]
async fn test_rtsp_warm_receives_frames() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let tc = env.thumbnail_client();
    ensure_data_flowing(&tc, &env.stream_source).await;

    let client = RtspClient::new(&env.mcm_rtsp_url, Codec::H264, None)
        .await
        .expect("rtsp client");
    scenario_warm_receives_frames(&client, Duration::from_secs(30), "RTSP").await;
}

/// Kill RTSP client and immediately reconnect (user pattern 3->4).
/// Must NOT get 503 Service Unavailable.
#[tokio::test]
async fn test_rtsp_immediate_reconnect_no_503() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let url = env.mcm_rtsp_url.clone();
    scenario_immediate_reconnect(
        &c,
        mon,
        || async { RtspClient::new(&url, Codec::H264, None).await.unwrap() },
        Duration::ZERO,
        "RTSP",
    )
    .await;
}

/// RTSP stream must stay alive as long as a client is connected (user
/// pattern: playback stops after ~170 frames when stream goes to Idle).
#[tokio::test]
async fn test_rtsp_stays_alive_while_connected() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let client = RtspClient::new(&env.mcm_rtsp_url, Codec::H264, None)
        .await
        .expect("rtsp client");
    scenario_stays_alive(&c, mon, &client, "RTSP").await;
}

/// After RTSP disconnects, stream returns to Idle.
#[tokio::test]
async fn test_rtsp_returns_to_idle() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let url = env.mcm_rtsp_url.clone();
    scenario_returns_to_idle(
        &c,
        || async { RtspClient::new(&url, Codec::H264, None).await.unwrap() },
        "RTSP",
    )
    .await;
}

/// Disconnect, wait for Idle, reconnect -- must work (user pattern 5->6).
#[tokio::test]
async fn test_rtsp_reconnect_after_idle() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let url = env.mcm_rtsp_url.clone();
    scenario_reconnect_after_idle(
        &c,
        &env.rest_url,
        || async { RtspClient::new(&url, Codec::H264, None).await.unwrap() },
        "RTSP",
    )
    .await;
}

/// Extended cycle test: connect/settle/disconnect 5 times from Idle.
/// Each connection must receive frames. Never go Stopped.
#[tokio::test]
async fn test_rtsp_extended_cycles() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let mon = env.monitor();

    let url = env.mcm_rtsp_url.clone();
    scenario_extended_cycles(
        &c,
        mon,
        || async { RtspClient::new(&url, Codec::H264, None).await.unwrap() },
        5,
        "RTSP",
    )
    .await;
}

/// Multi-warm sequential test: WebRTC keeps the pipeline running while
/// multiple RTSP clients connect/disconnect sequentially.
#[tokio::test]
async fn test_rtsp_multi_warm_sequential() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    ensure_idle(&c).await;

    let anchor = webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await;
    anchor
        .wait_for_frames(5, cold_timeout())
        .await
        .expect("WebRTC anchor must receive frames");
    eprintln!("WebRTC anchor established");

    let url = env.mcm_rtsp_url.clone();
    scenario_multi_warm_sequential(
        &anchor,
        || async { RtspClient::new(&url, Codec::H264, None).await.unwrap() },
        5,
        "WebRTC anchor",
        "RTSP",
    )
    .await;
}
