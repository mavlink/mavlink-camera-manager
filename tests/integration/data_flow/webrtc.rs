use super::*;

/// Create a fake stream (non-lazy), connect a WebRTC GStreamer client
/// via the signalling server, and verify actual media frames arrive.
async fn run_fake_webrtc_data_flow(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (name, path) = match codec {
        Codec::H264 => ("fake_webrtc", "test_webrtc"),
        Codec::H265 => ("fake_h265_webrtc", "test_h265_webrtc"),
        other => unreachable!("WebRTC only supports H264/H265, got {other:?}"),
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

    let signalling_url = mcm.signalling_url();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _webrtc =
        stream_clients::webrtc_client::WebrtcClient::connect(&signalling_url, None, Some(tx), None)
            .await
            .unwrap();

    verify_data_flow(&mut rx, &format!("{codec:?} WebRTC")).await;
}

#[tokio::test]
async fn test_fake_h264_webrtc_data_flow() {
    run_fake_webrtc_data_flow(Codec::H264).await;
}

#[tokio::test]
async fn test_fake_h265_webrtc_data_flow() {
    run_fake_webrtc_data_flow(Codec::H265).await;
}
