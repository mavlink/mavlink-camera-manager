use super::*;

#[tokio::test]
async fn test_idle_running_idle_cycle() {
    let (_mcm, client) = setup_fake_rtsp("idle_cycle", "idle_cycle").await;

    for cycle in 0..3 {
        wait_for_idle(&client).await;

        let resp = client.thumbnail("ball").await.unwrap();
        assert_eq!(resp.status(), 200, "thumbnail should work on cycle {cycle}");

        let streams = client.list_streams().await.unwrap();
        assert_eq!(
            streams[0].state,
            StreamStatusState::Running,
            "stream should be running after thumbnail on cycle {cycle}"
        );
    }
}

/// Kill the external UDP sender to induce a pipeline error on the redirect
/// stream, wait for the watcher to detect the failure, then restart the
/// sender and verify zenoh frames resume after the backoff/recovery cycle.
#[tokio::test(flavor = "multi_thread")]
async fn test_zenoh_recovery_after_pipeline_error() {
    gst::init().unwrap();

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_h264_udp_sender("127.0.0.1", udp_port);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let redirect = PostStream {
        name: "recovery_zenoh".to_string(),
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
        .wait_for_streams_running(1, Duration::from_secs(60))
        .await
        .unwrap_or_else(|e| {
            sender.kill().ok();
            panic!("redirect stream not running: {e}");
        });

    let topic = zenoh_topic("recovery_zenoh");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H264,
        Some(tx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    // Phase 1: verify zenoh frames are flowing
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if !drain_zenoh(&mut rx).is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "No initial Zenoh frames received within 30s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Phase 2: kill the sender to cause a pipeline error
    sender.kill().ok();
    let _ = sender.wait();

    // Wait for the pipeline to notice the loss and enter error/backoff.
    // The redirect pipeline should detect the missing source within a few
    // seconds. We drain any residual frames in the channel.
    tokio::time::sleep(Duration::from_secs(5)).await;
    drain_zenoh(&mut rx);

    // Phase 3: restart the sender on the same port
    let mut sender2 = spawn_h264_udp_sender("127.0.0.1", udp_port);
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Phase 4: wait for zenoh frames to resume (allow up to 60s for backoff)
    let recovery_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut recovered = false;
    while tokio::time::Instant::now() < recovery_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !drain_zenoh(&mut rx).is_empty() {
            recovered = true;
            break;
        }
    }

    assert!(
        recovered,
        "Zenoh frames did not resume after restarting the sender"
    );

    sender2.kill().ok();
}
