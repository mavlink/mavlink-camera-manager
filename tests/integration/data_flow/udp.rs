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
    let post = build_fake_udp_non_lazy(&name, 320, 240, 30, "127.0.0.1", udp_port, codec);
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let monitor = StateMonitor::start(&mcm.rest_url(), Duration::from_millis(250));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _udp = match codec {
        Codec::Yuyv | Codec::Rgb => stream_clients::udp_client::UdpClient::new_raw(
            "127.0.0.1",
            udp_port as i32,
            codec,
            320,
            240,
            Some(tx),
        )
        .unwrap(),
        _ => stream_clients::udp_client::UdpClient::new(
            "127.0.0.1",
            udp_port as i32,
            codec,
            Some(tx),
        )
        .unwrap(),
    };

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
