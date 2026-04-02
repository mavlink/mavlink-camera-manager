use super::*;

#[tokio::test]
async fn test_thumbnail_then_webrtc_cold() {
    let (mcm, client) = setup_fake_rtsp("mixed_tw", "mixed_tw").await;

    wait_for_idle(&client).await;

    // Thumbnail first (wakes the pipeline)
    let resp = client.thumbnail("ball").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Then WebRTC on the same pipeline (should be warm)
    let (bind, mut sink, _s) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

    let streams = client.list_streams().await.unwrap();
    assert_eq!(streams[0].state, StreamStatusState::Running);

    end_webrtc_session(&mut sink, &bind).await.unwrap();
}

#[tokio::test]
async fn test_webrtc_and_thumbnail_concurrent() {
    let (mcm, client) = setup_fake_rtsp("mixed_conc", "mixed_conc").await;

    // Start WebRTC session
    let (bind, mut sink, _s) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

    // Request thumbnail while WebRTC is active
    let resp = client.thumbnail("ball").await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "thumbnail should work while WebRTC is active"
    );

    // End WebRTC -- thumbnail cooldown should keep pipeline alive
    end_webrtc_session(&mut sink, &bind).await.unwrap();

    // Pipeline should still be running (thumbnail cooldown)
    tokio::time::sleep(Duration::from_secs(2)).await;
    let streams = client.list_streams().await.unwrap();
    assert_eq!(streams[0].state, StreamStatusState::Running);
}

#[tokio::test]
async fn test_webrtc_and_rtsp_independent() {
    let (mcm, client) = setup_fake_rtsp("mixed_indep", "mixed_indep").await;

    // Start WebRTC -- pipeline should be Running
    let (bind, mut sink, _s) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "pipeline should be running while WebRTC is active"
    );

    // RTSP server should respond to OPTIONS while WebRTC is active
    assert!(
        rtsp_options_ok(&mcm.rtsp_url("mixed_indep")).await,
        "RTSP server should accept connections while WebRTC is active"
    );

    // End WebRTC -- RTSP OPTIONS does not hold a media session, so no
    // lifecycle consumer remains.  Pipeline should drain to Idle.
    end_webrtc_session(&mut sink, &bind).await.unwrap();
    wait_for_idle(&client).await;
}

/// Removing WebRTC consumers must never freeze or interrupt the RTSP
/// stream. Regression test for the edge case where `remove_sink` posted
/// a spurious EOS message to the pipeline bus, killing the
/// PipelineRunner and triggering a full pipeline restart.
#[tokio::test]
async fn test_rtsp_not_frozen_after_removing_webrtc_consumers() {
    // Use disable_lazy so the pipeline stays Running throughout the test
    // and we can focus purely on the WebRTC removal → RTSP freeze bug.
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let post = McmClient::build_fake_h264_rtsp(
        "rtsp_wrtc_freeze",
        640,
        480,
        30,
        "rtsp_wrtc_freeze",
        Some(ExtendedConfiguration {
            disable_lazy: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let mon = StateMonitor::start(&mcm.rest_url(), Duration::from_millis(200));

    // 1. Connect an RTSP client and wait for frames to flow.
    let rtsp = RtspClient::new(&mcm.rtsp_url("rtsp_wrtc_freeze"), Codec::H264, None)
        .await
        .expect("RTSP client");
    rtsp.wait_for_frames(5, Duration::from_secs(30))
        .await
        .expect("RTSP must start receiving frames");
    eprintln!("[rtsp_wrtc_freeze] RTSP flowing: {} frames", rtsp.frames());

    // 2. Connect 2 WebRTC sessions
    let (bind1, mut sink1, _s1) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();
    let (bind2, mut sink2, _s2) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    eprintln!(
        "[rtsp_wrtc_freeze] 2 WebRTC sessions active, RTSP frames={}",
        rtsp.frames()
    );

    // 3. Remove both WebRTC sessions (simulates "remove all consumers")
    let frames_before = rtsp.frames();
    end_webrtc_session(&mut sink1, &bind1).await.unwrap();
    end_webrtc_session(&mut sink2, &bind2).await.unwrap();
    eprintln!(
        "[rtsp_wrtc_freeze] WebRTC sessions ended, frames_before_removal={}",
        frames_before
    );

    // 4. Verify RTSP continues to receive frames without any stall.
    //    A 2-second max stall catches the bug (the original freeze was
    //    ~30 s) while allowing for normal scheduling jitter.
    let final_frames = rtsp
        .wait_for_continuous_frames(Duration::from_secs(5), Duration::from_millis(500))
        .await
        .expect("RTSP frame flow must not stall after removing WebRTC consumers");

    assert!(
        final_frames > frames_before,
        "RTSP must keep receiving frames after WebRTC removal \
         (before={frames_before}, after={final_frames})"
    );

    // 5. Stream must have stayed Running the entire time
    let transitions = mon.stop();
    let ever_stopped = transitions
        .iter()
        .any(|(_, st)| *st == StreamStatusState::Idle);
    assert!(
        !ever_stopped,
        "Stream must never leave Running while RTSP client is connected, \
         transitions: {transitions:?}"
    );

    eprintln!("[rtsp_wrtc_freeze] PASS: {final_frames} total RTSP frames, no stall");
}
