use super::*;

#[tokio::test]
async fn test_thumbnail_warm_returns_200() {
    let (_mcm, client) = setup_fake_rtsp("thumb_warm", "thumb_warm").await;

    let resp = client.thumbnail("ball").await.unwrap();
    assert_eq!(resp.status(), 200, "warm thumbnail should return 200");
}

#[tokio::test]
async fn test_thumbnail_cold_start_returns_200() {
    let (_mcm, client) = setup_fake_rtsp("thumb_cold", "thumb_cold").await;

    wait_for_idle(&client).await;

    let resp = client.thumbnail("ball").await.unwrap();
    assert_eq!(resp.status(), 200, "cold-start thumbnail should return 200");

    // Stream should be back to Running after thumbnail wakes it
    let streams = client.list_streams().await.unwrap();
    assert_eq!(streams[0].state, StreamStatusState::Running);
}

#[tokio::test]
async fn test_thumbnail_rapid_sequence() {
    let (_mcm, client) = setup_fake_rtsp("thumb_rapid", "thumb_rapid").await;

    wait_for_idle(&client).await;

    // The thumbnail endpoint has a rate limit of 4 req/s per IP.
    // Space requests 350 ms apart to stay safely within the limit.
    for i in 0..5 {
        let resp = client.thumbnail("ball").await.unwrap();
        assert_eq!(
            resp.status(),
            200,
            "thumbnail attempt {i} should return 200"
        );
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
}

#[tokio::test]
async fn test_thumbnail_keeps_pipeline_alive_during_cooldown() {
    let (_mcm, client) = setup_fake_rtsp("thumb_cooldown", "thumb_cooldown").await;

    wait_for_idle(&client).await;

    // Request a thumbnail to wake the pipeline
    let resp = client.thumbnail("ball").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Wait less than the thumbnail cooldown (15s) but more than
    // the idle grace period (5s). The pipeline should still be Running
    // because the cooldown prevents re-suspension.
    tokio::time::sleep(IDLE_GRACE + Duration::from_secs(2)).await;

    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "thumbnail cooldown should prevent re-suspension"
    );
}
