use super::*;

/// Create a fake stream with a UDP endpoint (non-lazy), connect a UDP
/// GStreamer client, and verify actual RTP frames arrive continuously for
/// longer than the idle grace period.
async fn run_fake_udp_data_flow(codec: Codec) {
    gst::init().unwrap();

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let name = format!("fake_{codec:?}_udp").to_lowercase();
    let post = McmClient::build_fake_udp(
        codec,
        &name,
        320,
        240,
        30,
        "127.0.0.1",
        udp_port,
        Some(NON_LAZY),
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let monitor = StateMonitor::start(&mcm.rest_url(), Duration::from_millis(250));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let dimensions = match codec {
        Codec::Yuyv | Codec::Rgb => Some((320, 240)),
        _ => None,
    };
    let _udp =
        stream_clients::udp_client::UdpClient::new(stream_clients::udp_client::UdpClientConfig {
            address: "127.0.0.1",
            port: udp_port,
            codec,
            sender: Some(tx),
            dimensions,
        })
        .unwrap();

    let label = format!("{codec:?} UDP");
    verify_data_flow(&mut rx, &label).await;

    let transitions = monitor.stop();
    verify_never_idle(&transitions, &label);
}

#[tokio::test]
async fn test_fake_h264_udp_data_flow() {
    run_fake_udp_data_flow(Codec::H264).await;
}

#[tokio::test]
async fn test_fake_h265_udp_data_flow() {
    run_fake_udp_data_flow(Codec::H265).await;
}

#[tokio::test]
async fn test_fake_mjpg_udp_data_flow() {
    run_fake_udp_data_flow(Codec::Mjpg).await;
}

#[tokio::test]
async fn test_fake_yuyv_udp_data_flow() {
    run_fake_udp_data_flow(Codec::Yuyv).await;
}
