use std::time::Duration;

use super::*;

/// WebRTC + thumbnail concurrent: both work, WebRTC keeps getting frames.
#[tokio::test]
async fn test_mixed_webrtc_and_thumbnail() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let wrtc = webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await;
    wrtc.wait_for_frames(3, warm_timeout())
        .await
        .expect("webrtc frames");

    let resp = thumbnail_with_retry(&tc, &env.stream_source).await;
    assert_eq!(resp.status(), 200, "thumbnail while webrtc active");
    let body = resp.bytes().await.unwrap();
    assert!(body.len() > 1000);

    let before = wrtc.frames();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        wrtc.frames() > before,
        "webrtc must keep receiving frames after thumbnail"
    );

    let transitions = mon.stop();
    assert_never_stopped(&transitions, "mixed webrtc+thumbnail");
}

/// Full lifecycle: WebRTC → thumbnail → stop both → idle → thumbnail → WebRTC.
/// This is the full 7-step user regression.
#[tokio::test]
async fn test_mixed_full_lifecycle() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();
    ensure_idle(&c).await;
    eprintln!("[STEP 0] confirmed idle");

    let mon = env.monitor();

    let wrtc = webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await;
    wrtc.wait_for_frames(5, warm_timeout())
        .await
        .expect("step1: webrtc must receive frames");
    eprintln!("[STEP 1] cold WebRTC ✓ ({} frames)", wrtc.frames());

    for i in 0..5 {
        let resp = thumbnail_with_retry(&tc, &env.stream_source).await;
        assert_eq!(resp.status(), 200, "step2: thumbnail #{i}");
        let body = resp.bytes().await.unwrap();
        assert!(body.len() > 1000, "step2: thumbnail #{i} too small");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    eprintln!("[STEP 2] thumbnails while WebRTC active ✓");

    drop(wrtc);
    eprintln!("[STEP 3] WebRTC dropped");

    eprintln!("[STEP 4] thumbnails stopped");

    ensure_idle(&c).await;
    eprintln!("[STEP 5] confirmed idle");

    let body = cold_thumbnail(&tc, cold_timeout(), &env.stream_source).await;
    assert!(body.len() > 1000, "step6: thumbnail body too small");
    eprintln!("[STEP 6] thumbnail after idle ✓ ({} bytes)", body.len());

    let wrtc2 = webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await;
    ensure_running(&c).await;
    wrtc2
        .wait_for_frames(5, warm_timeout())
        .await
        .expect("step7: webrtc must receive frames");
    eprintln!("[STEP 7] WebRTC after idle ✓ ({} frames)", wrtc2.frames());

    let transitions = mon.stop();
    eprintln!("full lifecycle transitions: {}", states_str(&transitions));
    assert_never_stopped(&transitions, "full lifecycle");
}

/// RTSP + thumbnail keeps stream alive (user pattern: RTSP stops after
/// ~170 frames, but requesting a thumbnail restores playback).
#[tokio::test]
async fn test_mixed_rtsp_plus_thumbnail_keeps_alive() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let rtsp = RtspClient::new(&env.mcm_rtsp_url, Codec::H264, None)
        .await
        .expect("rtsp client");
    ensure_running(&c).await;
    rtsp.wait_for_frames(5, warm_timeout())
        .await
        .expect("rtsp must start");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut consecutive_503 = 0u32;
    while tokio::time::Instant::now() < deadline {
        let resp = thumbnail_with_retry(&tc, &env.stream_source).await;
        if resp.status() == 200 {
            consecutive_503 = 0;
        } else {
            consecutive_503 += 1;
            assert!(
                consecutive_503 < 3,
                "thumbnail returned non-200 ({}) 3 times in a row while rtsp active",
                resp.status()
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let before = rtsp.frames();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = rtsp.frames();
    assert!(
        after > before,
        "RTSP must keep getting frames during thumbnail coexistence (before={before}, after={after})"
    );

    let transitions = mon.stop();
    eprintln!(
        "rtsp+thumb: {} frames, transitions: {}",
        after,
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, "rtsp+thumbnail coexistence");
}
