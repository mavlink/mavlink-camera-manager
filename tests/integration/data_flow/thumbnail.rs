use super::*;

/// Create a fake stream (non-lazy) and request a thumbnail, verifying
/// a non-empty JPEG image is returned.
async fn run_fake_thumbnail_data_flow(codec: Codec) {
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (name, path) = match codec {
        Codec::H264 => ("fake_thumbnail", "test_thumb"),
        Codec::H265 => ("fake_h265_thumbnail", "test_h265_thumb"),
        Codec::Mjpg => ("fake_mjpg_thumbnail", "test_mjpg_thumb"),
        Codec::Yuyv => ("fake_yuyv_thumbnail", "test_yuyv_thumb"),
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

    let body = wait_for_thumbnail(&client, "ball", TIMEOUT).await;
    assert!(
        body.len() > 100,
        "Thumbnail too small ({} bytes), expected a JPEG image",
        body.len()
    );
    assert_eq!(&body[..2], &[0xFF, 0xD8], "Thumbnail is not a JPEG image");
}

#[tokio::test]
async fn test_fake_h264_thumbnail_data_flow() {
    run_fake_thumbnail_data_flow(Codec::H264).await;
}

#[tokio::test]
async fn test_fake_h265_thumbnail_data_flow() {
    run_fake_thumbnail_data_flow(Codec::H265).await;
}

#[tokio::test]
async fn test_fake_mjpg_thumbnail_data_flow() {
    run_fake_thumbnail_data_flow(Codec::Mjpg).await;
}

#[tokio::test]
async fn test_fake_yuyv_thumbnail_data_flow() {
    run_fake_thumbnail_data_flow(Codec::Yuyv).await;
}
