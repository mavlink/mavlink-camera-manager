use super::*;

#[tokio::test]
async fn test_rtsp_cold_connection() {
    let (mcm, client) = setup_fake_rtsp("rtsp_cold", "rtsp_cold").await;

    wait_for_idle(&client).await;

    // Connect an RTSP client -- this should wake the pipeline via the RTSP
    // factory's on_client_connected callback.
    assert!(
        rtsp_options_ok(&mcm.rtsp_url("rtsp_cold")).await,
        "RTSP server should accept connections even when pipeline is idle"
    );
}

#[tokio::test]
async fn test_rtsp_warm_connection() {
    let (mcm, _client) = setup_fake_rtsp("rtsp_warm", "rtsp_warm").await;

    assert!(
        rtsp_options_ok(&mcm.rtsp_url("rtsp_warm")).await,
        "RTSP server should respond while pipeline is running"
    );
}
