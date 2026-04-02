use super::*;

/// Create a fake RTSP stream (non-lazy), connect an RTSP GStreamer
/// client, and verify actual decoded frames arrive continuously.
async fn run_fake_rtsp_data_flow(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (name, path) = match codec {
        Codec::H264 => ("fake_rtsp", "test_rtsp"),
        Codec::H265 => ("fake_h265_rtsp", "test_h265_rtsp"),
        Codec::Mjpg => ("fake_mjpg_rtsp", "test_mjpg_rtsp"),
        Codec::Yuyv => ("fake_yuyv_rtsp", "test_yuyv_rtsp"),
        Codec::Rgb => unreachable!("Fake pipeline does not support RGB"),
    };
    let post = build_fake_rtsp(
        codec,
        name,
        320,
        240,
        30,
        path,
        Some(NON_LAZY),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let rtsp_url = mcm.rtsp_url(path);
    wait_for_rtsp_tcp(&rtsp_url, TIMEOUT).await;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _rtsp = stream_clients::rtsp_client::RtspClient::new(&rtsp_url, codec, Some(tx))
        .await
        .unwrap();

    verify_data_flow(&mut rx, &format!("{codec:?} RTSP")).await;
}

#[tokio::test]
async fn test_fake_h264_rtsp_data_flow() {
    run_fake_rtsp_data_flow(Codec::H264).await;
}

#[tokio::test]
async fn test_fake_h265_rtsp_data_flow() {
    run_fake_rtsp_data_flow(Codec::H265).await;
}

#[tokio::test]
async fn test_fake_mjpg_rtsp_data_flow() {
    run_fake_rtsp_data_flow(Codec::Mjpg).await;
}

#[tokio::test]
async fn test_fake_yuyv_rtsp_data_flow() {
    run_fake_rtsp_data_flow(Codec::Yuyv).await;
}
