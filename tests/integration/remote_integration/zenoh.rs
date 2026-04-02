use super::*;

/// Verify that a zenoh subscriber receives video frames from the MCM
/// stream. Only runs in Fake mode (local MCM with zenoh enabled).
#[tokio::test(flavor = "multi_thread")]
async fn test_zenoh_warm_data_flow() {
    skip_unless!(SourceTag::Fake);

    let env = TestEnv::setup_with_zenoh().await;

    let topic = zenoh_topic("test_stream");
    let zenoh = stream_clients::zenoh_client::ZenohClient::new(
        &topic,
        Codec::H264,
        None,
        env.zenoh_config(),
    )
    .await
    .unwrap();

    zenoh
        .wait_for_frames(5, warm_timeout())
        .await
        .expect("Expected at least 5 Zenoh frames");
}
