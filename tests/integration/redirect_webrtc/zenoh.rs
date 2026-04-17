use super::*;

/// External gst-launch UDP sender -> redirect stream (zenoh enabled) ->
/// zenoh client receives video frames.
#[tokio::test(flavor = "multi_thread")]
async fn test_udp_redirect_zenoh_data_flow() {
    gst::init().unwrap();

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_h264_udp_sender("127.0.0.1", udp_port);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let redirect = PostStream {
        name: "redirect_zenoh".to_string(),
        source: "Redirect".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![url::Url::parse(&format!("udp://127.0.0.1:{udp_port}")).unwrap()],
            configuration: CaptureConfiguration::Redirect {},
            extended_configuration: Some(ExtendedConfiguration {
                disable_mavlink: true,
                ..Default::default()
            }),
        },
    };
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create redirect stream: {e}");
    });

    client
        .wait_for_streams_running(1, TIMEOUT)
        .await
        .unwrap_or_else(|e| {
            sender.kill().ok();
            panic!("redirect stream not running: {e}");
        });

    let topic = zenoh_topic("redirect_zenoh");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H264,
        Some(tx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_zenoh_frame(&mut rx, TIMEOUT, "UDP redirect Zenoh").await;

    let mut count = 1usize;
    let window_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < window_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        count += drain_zenoh(&mut rx).len();
    }

    assert!(
        count >= 5,
        "Expected at least 5 Zenoh frames from UDP redirect, got {count}"
    );

    sender.kill().ok();
}

/// Fake RTSP source -> redirect RTSP stream (zenoh enabled) ->
/// zenoh client receives video frames.
#[tokio::test(flavor = "multi_thread")]
async fn test_rtsp_redirect_zenoh_data_flow() {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let path = "test_redir_zenoh";

    let fake = McmClient::build_fake_h264_rtsp(
        "fake_rtsp_sender",
        160,
        120,
        30,
        path,
        Some(ExtendedConfiguration {
            disable_mavlink: true,
            disable_zenoh: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&fake).await.unwrap();
    client
        .wait_for_stream_idle("fake_rtsp_sender", TIMEOUT)
        .await
        .expect("fake RTSP sender should complete initial lifecycle");
    mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

    let redirect = PostStream {
        name: "redirect_zenoh_rtsp".to_string(),
        source: "Redirect".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![
                url::Url::parse(&format!("rtsp://127.0.0.1:{}/{path}", mcm.rtsp_port)).unwrap(),
            ],
            configuration: CaptureConfiguration::Redirect {},
            extended_configuration: Some(ExtendedConfiguration {
                disable_mavlink: true,
                ..Default::default()
            }),
        },
    };
    client.create_stream(&redirect).await.unwrap();

    client
        .wait_for_streams_running(2, TIMEOUT)
        .await
        .expect("both streams should be running");

    let topic = zenoh_topic("redirect_zenoh_rtsp");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H264,
        Some(tx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_zenoh_frame(&mut rx, TIMEOUT, "RTSP redirect Zenoh").await;

    let mut count = 1usize;
    let window_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < window_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        count += drain_zenoh(&mut rx).len();
    }

    assert!(
        count >= 5,
        "Expected at least 5 Zenoh frames from RTSP redirect, got {count}"
    );
}

/// External H265 gst-launch UDP sender -> redirect stream (zenoh enabled) ->
/// zenoh client receives H265 video frames.
#[tokio::test(flavor = "multi_thread")]
async fn test_h265_udp_redirect_zenoh_data_flow() {
    gst::init().unwrap();

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_h265_udp_sender("127.0.0.1", udp_port);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let redirect = PostStream {
        name: "redirect_h265_zenoh".to_string(),
        source: "Redirect".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![url::Url::parse(&format!("udp://127.0.0.1:{udp_port}")).unwrap()],
            configuration: CaptureConfiguration::Redirect {},
            extended_configuration: Some(ExtendedConfiguration {
                disable_mavlink: true,
                ..Default::default()
            }),
        },
    };
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create H265 redirect stream: {e}");
    });

    client
        .wait_for_streams_running(1, TIMEOUT)
        .await
        .unwrap_or_else(|e| {
            sender.kill().ok();
            panic!("H265 redirect stream not running: {e}");
        });

    let topic = zenoh_topic("redirect_h265_zenoh");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H265,
        Some(tx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_zenoh_frame(&mut rx, TIMEOUT, "H265 UDP redirect Zenoh").await;

    let mut count = 1usize;
    let window_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < window_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        count += drain_zenoh(&mut rx).len();
    }

    assert!(
        count >= 5,
        "Expected at least 5 H265 Zenoh frames from UDP redirect, got {count}"
    );

    sender.kill().ok();
}

/// Fake H265 RTSP source -> redirect RTSP stream (zenoh enabled) ->
/// zenoh client receives H265 video frames.
#[tokio::test(flavor = "multi_thread")]
async fn test_h265_rtsp_redirect_zenoh_data_flow() {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let path = "test_h265_redir_zenoh";

    let fake = McmClient::build_fake_h265_rtsp(
        "fake_h265_rtsp_sender",
        160,
        120,
        30,
        path,
        Some(ExtendedConfiguration {
            disable_mavlink: true,
            disable_zenoh: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&fake).await.unwrap();
    client
        .wait_for_stream_idle("fake_h265_rtsp_sender", TIMEOUT)
        .await
        .expect("fake H265 RTSP sender should complete initial lifecycle");
    mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

    let redirect = PostStream {
        name: "redirect_h265_zenoh_rtsp".to_string(),
        source: "Redirect".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![
                url::Url::parse(&format!("rtsp://127.0.0.1:{}/{path}", mcm.rtsp_port)).unwrap(),
            ],
            configuration: CaptureConfiguration::Redirect {},
            extended_configuration: Some(ExtendedConfiguration {
                disable_mavlink: true,
                ..Default::default()
            }),
        },
    };
    client.create_stream(&redirect).await.unwrap();

    client
        .wait_for_streams_running(2, TIMEOUT)
        .await
        .expect("both streams should be running");

    let topic = zenoh_topic("redirect_h265_zenoh_rtsp");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H265,
        Some(tx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    wait_first_zenoh_frame(&mut rx, TIMEOUT, "H265 RTSP redirect Zenoh").await;

    let mut count = 1usize;
    let window_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < window_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        count += drain_zenoh(&mut rx).len();
    }

    assert!(
        count >= 5,
        "Expected at least 5 H265 Zenoh frames from RTSP redirect, got {count}"
    );
}
