use super::*;

// QR pipeline tests require the `qrtimestampsrc` GStreamer plugin.

async fn run_qr_rtsp_data_flow(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (name, path) = match codec {
        Codec::H264 => ("qr_h264_rtsp", "test_qr_h264_rtsp"),
        Codec::Rgb => ("qr_rgb_rtsp", "test_qr_rgb_rtsp"),
        other => unreachable!("QR pipeline does not support {other:?}"),
    };
    let post = match codec {
        Codec::H264 => {
            McmClient::build_qr_h264_rtsp(name, 320, 30, path, Some(NON_LAZY), mcm.rtsp_port)
        }
        Codec::Rgb => {
            McmClient::build_qr_rgb_rtsp(name, 320, 30, path, Some(NON_LAZY), mcm.rtsp_port)
        }
        other => unreachable!("QR pipeline does not support {other:?}"),
    };
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let rtsp_url = mcm.rtsp_url(path);
    wait_for_rtsp_tcp(&rtsp_url, TIMEOUT).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _rtsp = stream_clients::rtsp_client::RtspClient::new(&rtsp_url, codec, Some(tx))
        .await
        .unwrap();

    verify_data_flow(&mut rx, &format!("QR {codec:?} RTSP")).await;
}

async fn run_qr_udp_data_flow(codec: Codec) {
    gst::init().unwrap();

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let name = format!("qr_{codec:?}_udp").to_lowercase();
    let post = match codec {
        Codec::H264 => McmClient::build_qr_h264_udp(&name, 320, 30, "127.0.0.1", udp_port),
        Codec::Rgb => McmClient::build_qr_rgb_udp(&name, 320, 30, "127.0.0.1", udp_port),
        other => unreachable!("QR pipeline does not support {other:?}"),
    };
    let mut post = post;
    post.stream_information.extended_configuration = Some(NON_LAZY);
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let monitor = StateMonitor::start(&mcm.rest_url(), Duration::from_millis(250));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _udp = match codec {
        Codec::Rgb => stream_clients::udp_client::UdpClient::new_raw(
            "127.0.0.1",
            udp_port as i32,
            codec,
            320,
            320,
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

    let label = format!("QR {codec:?} UDP");
    verify_data_flow(&mut rx, &label).await;

    let transitions = monitor.stop();
    verify_never_idle(&transitions, &label);
}

async fn run_qr_thumbnail_data_flow(codec: Codec) {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (name, path) = match codec {
        Codec::H264 => ("qr_h264_thumb", "test_qr_h264_thumb"),
        Codec::Rgb => ("qr_rgb_thumb", "test_qr_rgb_thumb"),
        other => unreachable!("QR pipeline does not support {other:?}"),
    };
    let post = match codec {
        Codec::H264 => {
            McmClient::build_qr_h264_rtsp(name, 320, 30, path, Some(NON_LAZY), mcm.rtsp_port)
        }
        Codec::Rgb => {
            McmClient::build_qr_rgb_rtsp(name, 320, 30, path, Some(NON_LAZY), mcm.rtsp_port)
        }
        other => unreachable!("QR pipeline does not support {other:?}"),
    };
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let body = wait_for_thumbnail(&client, "QRTimeStamp", TIMEOUT).await;
    assert!(
        body.len() > 100,
        "Thumbnail too small ({} bytes), expected a JPEG image",
        body.len()
    );
    assert_eq!(&body[..2], &[0xFF, 0xD8], "Thumbnail is not a JPEG image");
}

async fn run_qr_webrtc_data_flow() {
    gst::init().unwrap();

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let post = McmClient::build_qr_h264_rtsp(
        "qr_webrtc",
        320,
        30,
        "test_qr_webrtc",
        Some(NON_LAZY),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let signalling_url = mcm.signalling_url();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _webrtc =
        stream_clients::webrtc_client::WebrtcClient::connect(&signalling_url, None, Some(tx), None)
            .await
            .unwrap();

    verify_data_flow(&mut rx, "QR H264 WebRTC").await;
}

async fn run_qr_zenoh_data_flow() {
    gst::init().unwrap();

    let mcm = McmProcess::start_with_zenoh().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let stream_name = "qr_zenoh";
    let post = McmClient::build_qr_h264_rtsp(
        stream_name,
        320,
        30,
        "test_qr_zenoh",
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
    let _zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H264,
        Some(tx),
        mcm.zenoh_config(),
    )
    .await
    .unwrap();

    verify_data_flow(&mut rx, "QR H264 Zenoh").await;

    let transitions = monitor.stop();
    verify_never_idle(&transitions, "QR H264 Zenoh");
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_h264_rtsp_data_flow() {
    run_qr_rtsp_data_flow(Codec::H264).await;
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_rgb_rtsp_data_flow() {
    run_qr_rtsp_data_flow(Codec::Rgb).await;
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_h264_udp_data_flow() {
    run_qr_udp_data_flow(Codec::H264).await;
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_rgb_udp_data_flow() {
    run_qr_udp_data_flow(Codec::Rgb).await;
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_h264_webrtc_data_flow() {
    run_qr_webrtc_data_flow().await;
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_h264_thumbnail_data_flow() {
    run_qr_thumbnail_data_flow(Codec::H264).await;
}

#[tokio::test]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_rgb_thumbnail_data_flow() {
    run_qr_thumbnail_data_flow(Codec::Rgb).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires qrtimestampsrc GStreamer plugin"]
async fn test_qr_h264_zenoh_data_flow() {
    run_qr_zenoh_data_flow().await;
}
