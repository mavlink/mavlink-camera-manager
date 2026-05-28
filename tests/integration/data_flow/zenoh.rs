use super::*;

/// Create a fake stream with zenoh enabled (non-lazy), connect a Zenoh
/// subscriber client, and verify actual video frames arrive continuously for
/// longer than the idle grace period.
async fn run_fake_zenoh_data_flow(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (stream_name, path) = match codec {
        Codec::H264 => ("fake_zenoh", "test_zenoh"),
        Codec::H265 => ("fake_h265_zenoh", "test_h265_zenoh"),
        other => unreachable!("Zenoh only supports H264/H265, got {other:?}"),
    };
    let post = build_fake_rtsp(
        codec,
        stream_name,
        320,
        240,
        30,
        path,
        Some(ExtendedConfiguration {
            disable_mavlink: true,
            disable_lazy: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let monitor = StateMonitor::start(&mcm.rest_url(), Duration::from_millis(250));

    let topic = zenoh_topic(stream_name);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh =
        stream_clients::zenoh_client::ZenohClient::new(&topic, codec, Some(tx), mcm.zenoh_config())
            .await
            .unwrap();

    let label = format!("{codec:?} Zenoh");
    verify_data_flow(&mut rx, &label).await;

    let transitions = monitor.stop();
    verify_never_idle(&transitions, &label);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_fake_h264_zenoh_data_flow() {
    run_fake_zenoh_data_flow(Codec::H264).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_fake_h265_zenoh_data_flow() {
    run_fake_zenoh_data_flow(Codec::H265).await;
}

/// Verify zenoh and WebRTC can receive data simultaneously on the same stream.
/// After disconnecting WebRTC, zenoh must continue to receive frames.
async fn run_zenoh_and_webrtc_coexistence(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (stream_name, path) = match codec {
        Codec::H264 => ("zenoh_webrtc_coex", "test_coex_wrtc"),
        Codec::H265 => ("zenoh_h265_webrtc_coex", "test_h265_coex_wrtc"),
        other => unreachable!("Zenoh only supports H264/H265, got {other:?}"),
    };
    let post = build_fake_rtsp(
        codec,
        stream_name,
        320,
        240,
        30,
        path,
        Some(ExtendedConfiguration {
            disable_mavlink: true,
            disable_lazy: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let topic = zenoh_topic(stream_name);
    let (ztx, mut zrx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        codec,
        Some(ztx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_frame(&mut zrx, TIMEOUT, "Zenoh (coex)").await;

    let signalling_url = mcm.signalling_url();
    let (wtx, mut wrx) = mpsc::unbounded_channel();
    let webrtc = stream_clients::webrtc_client::WebrtcClient::connect(
        &signalling_url,
        None,
        Some(wtx),
        None,
    )
    .await
    .unwrap();

    wait_first_frame(&mut wrx, TIMEOUT, "WebRTC (coex)").await;

    // Both should receive frames concurrently
    let zenoh_concurrent = collect_frames(&mut zrx, Duration::from_secs(4), MAX_FRAME_GAP).await;
    assert!(
        zenoh_concurrent.len() >= 5,
        "Zenoh should keep receiving while WebRTC is active, got {}",
        zenoh_concurrent.len()
    );

    // Disconnect WebRTC
    drop(webrtc);

    // Zenoh must continue to flow after WebRTC disconnect
    let zenoh_after = collect_frames(&mut zrx, Duration::from_secs(4), MAX_FRAME_GAP).await;
    assert!(
        zenoh_after.len() >= 5,
        "Zenoh should keep flowing after WebRTC disconnect, got {}",
        zenoh_after.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h264_zenoh_and_webrtc_coexistence() {
    run_zenoh_and_webrtc_coexistence(Codec::H264).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h265_zenoh_and_webrtc_coexistence() {
    run_zenoh_and_webrtc_coexistence(Codec::H265).await;
}

/// Verify zenoh and RTSP can receive data simultaneously on the same stream.
/// After disconnecting the RTSP client, zenoh must continue to receive frames.
async fn run_zenoh_and_rtsp_coexistence(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (stream_name, path) = match codec {
        Codec::H264 => ("zenoh_rtsp_coex", "test_coex_rtsp"),
        Codec::H265 => ("zenoh_h265_rtsp_coex", "test_h265_coex_rtsp"),
        other => unreachable!("Zenoh only supports H264/H265, got {other:?}"),
    };
    let post = build_fake_rtsp(
        codec,
        stream_name,
        320,
        240,
        30,
        path,
        Some(ExtendedConfiguration {
            disable_mavlink: true,
            disable_lazy: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let topic = zenoh_topic(stream_name);
    let (ztx, mut zrx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        codec,
        Some(ztx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_frame(&mut zrx, TIMEOUT, "Zenoh (rtsp-coex)").await;

    let rtsp_url = mcm.rtsp_url(path);
    wait_for_rtsp_tcp(&rtsp_url, TIMEOUT).await;

    let (rtx, mut rrx) = mpsc::unbounded_channel();
    let rtsp = stream_clients::rtsp_client::RtspClient::new(&rtsp_url, codec, Some(rtx))
        .await
        .unwrap();

    wait_first_frame(&mut rrx, TIMEOUT, "RTSP (coex)").await;

    let zenoh_concurrent = collect_frames(&mut zrx, Duration::from_secs(4), MAX_FRAME_GAP).await;
    assert!(
        zenoh_concurrent.len() >= 5,
        "Zenoh should keep receiving while RTSP is active, got {}",
        zenoh_concurrent.len()
    );

    drop(rtsp);

    let zenoh_after = collect_frames(&mut zrx, Duration::from_secs(4), MAX_FRAME_GAP).await;
    assert!(
        zenoh_after.len() >= 5,
        "Zenoh should keep flowing after RTSP disconnect, got {}",
        zenoh_after.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h264_zenoh_and_rtsp_coexistence() {
    run_zenoh_and_rtsp_coexistence(Codec::H264).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h265_zenoh_and_rtsp_coexistence() {
    run_zenoh_and_rtsp_coexistence(Codec::H265).await;
}

/// Verify that `disable_zenoh: true` prevents any zenoh frames from being
/// published, even when the MCM process has zenoh globally enabled.
async fn run_disable_zenoh_produces_no_frames(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (stream_name, path) = match codec {
        Codec::H264 => ("no_zenoh", "test_no_zenoh"),
        Codec::H265 => ("no_h265_zenoh", "test_no_h265_zenoh"),
        other => unreachable!("Zenoh only supports H264/H265, got {other:?}"),
    };
    let post = build_fake_rtsp(
        codec,
        stream_name,
        320,
        240,
        30,
        path,
        Some(ExtendedConfiguration {
            disable_mavlink: true,
            disable_lazy: true,
            disable_zenoh: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let topic = zenoh_topic(stream_name);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh =
        stream_clients::zenoh_client::ZenohClient::new(&topic, codec, Some(tx), mcm.zenoh_config())
            .await
            .unwrap();

    tokio::time::sleep(Duration::from_secs(5)).await;

    let frames = drain(&mut rx);
    assert!(
        frames.is_empty(),
        "disable_zenoh stream should produce zero zenoh frames, got {}",
        frames.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h264_disable_zenoh_produces_no_frames() {
    run_disable_zenoh_produces_no_frames(Codec::H264).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h265_disable_zenoh_produces_no_frames() {
    run_disable_zenoh_produces_no_frames(Codec::H265).await;
}

/// Verify that two zenoh-enabled streams can publish to different topics
/// simultaneously and both subscribers receive frames independently.
async fn run_multiple_zenoh_streams(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (name_a, path_a, name_b, path_b) = match codec {
        Codec::H264 => (
            "multi_zenoh_a",
            "test_multi_zenoh_a",
            "multi_zenoh_b",
            "test_multi_zenoh_b",
        ),
        Codec::H265 => (
            "multi_h265_zenoh_a",
            "test_multi_h265_zenoh_a",
            "multi_h265_zenoh_b",
            "test_multi_h265_zenoh_b",
        ),
        other => unreachable!("Zenoh only supports H264/H265, got {other:?}"),
    };

    let ext = ExtendedConfiguration {
        disable_mavlink: true,
        disable_lazy: true,
        ..Default::default()
    };
    let post_a = build_fake_rtsp(
        codec,
        name_a,
        320,
        240,
        30,
        path_a,
        Some(ext.clone()),
        mcm.rtsp_port,
    );
    let post_b = build_fake_rtsp(
        codec,
        name_b,
        320,
        240,
        30,
        path_b,
        Some(ext),
        mcm.rtsp_port,
    );
    client.create_stream(&post_a).await.unwrap();
    client.create_stream(&post_b).await.unwrap();
    client.wait_for_streams_running(2, TIMEOUT).await.unwrap();

    let topic_a = zenoh_topic(name_a);
    let topic_b = zenoh_topic(name_b);

    let (tx_a, mut rx_a) = mpsc::unbounded_channel();
    let _zenoh_a = stream_clients::zenoh_client::ZenohClient::new(
        &topic_a,
        codec,
        Some(tx_a),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    let (tx_b, mut rx_b) = mpsc::unbounded_channel();
    let _zenoh_b = stream_clients::zenoh_client::ZenohClient::new(
        &topic_b,
        codec,
        Some(tx_b),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_frame(&mut rx_a, TIMEOUT, "Zenoh stream A").await;
    wait_first_frame(&mut rx_b, TIMEOUT, "Zenoh stream B").await;

    let samples_a = collect_frames(&mut rx_a, MEASUREMENT_WINDOW, MAX_FRAME_GAP).await;
    let samples_b = collect_frames(&mut rx_b, MEASUREMENT_WINDOW, MAX_FRAME_GAP).await;

    assert!(
        samples_a.len() >= MIN_FRAME_COUNT,
        "Stream A: expected at least {MIN_FRAME_COUNT} frames, got {}",
        samples_a.len()
    );
    assert!(
        samples_b.len() >= MIN_FRAME_COUNT,
        "Stream B: expected at least {MIN_FRAME_COUNT} frames, got {}",
        samples_b.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h264_multiple_zenoh_streams() {
    run_multiple_zenoh_streams(Codec::H264).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_h265_multiple_zenoh_streams() {
    run_multiple_zenoh_streams(Codec::H265).await;
}
