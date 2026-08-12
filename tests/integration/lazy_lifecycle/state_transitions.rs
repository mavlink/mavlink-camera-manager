use super::*;

#[tokio::test]
async fn test_stream_becomes_idle_after_grace_period() {
    let (_mcm, client) = setup_fake_rtsp("idle_test", "idle_test").await;

    // Stream is Running right after creation
    let streams = client.list_streams().await.unwrap();
    assert_eq!(streams[0].state, StreamStatusState::Running);

    wait_for_idle(&client).await;
}

#[tokio::test]
async fn test_udp_stream_never_goes_idle() {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let post = McmClient::build_fake_h264_udp("udp_no_idle", 640, 480, 30, "127.0.0.1", udp_port);
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    // Wait longer than the idle grace period
    tokio::time::sleep(IDLE_WAIT + Duration::from_secs(5)).await;

    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "UDP stream must never go idle"
    );
}

#[tokio::test]
async fn test_zenoh_stream_never_goes_idle() {
    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let post = McmClient::build_fake_h264_rtsp(
        "zenoh_no_idle",
        640,
        480,
        30,
        "zenoh_no_idle",
        None,
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    // Wait longer than the idle grace period
    tokio::time::sleep(IDLE_WAIT + Duration::from_secs(5)).await;

    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "Zenoh stream must never go idle"
    );
}

#[tokio::test]
async fn test_disable_lazy_prevents_idle() {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let post = McmClient::build_fake_h264_rtsp(
        "no_lazy",
        640,
        480,
        30,
        "no_lazy",
        Some(ExtendedConfiguration {
            disable_lazy: true,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    tokio::time::sleep(IDLE_WAIT + Duration::from_secs(5)).await;

    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "disable_lazy stream must never go idle"
    );
}
