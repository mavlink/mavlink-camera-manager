use std::time::Duration;

use super::*;

/// Pattern 2 from user: cold thumbnail should work and not go through Stopped.
/// Stream transitions must be Idle → Running (→ Idle), never Stopped.
#[tokio::test]
async fn test_thumb_cold_no_stopped_state() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();
    ensure_idle(&c).await;

    let mon = env.monitor();
    let body = cold_thumbnail(&tc, cold_timeout(), &env.stream_source).await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    let transitions = mon.stop();

    eprintln!(
        "cold thumbnail: {} bytes, transitions: {}",
        body.len(),
        states_str(&transitions)
    );

    assert!(
        body.len() > 1000,
        "thumbnail body too small: {} bytes",
        body.len()
    );
    assert_never_stopped(&transitions, "cold thumbnail");
}

/// Warm thumbnail: pipeline is already running, thumbnail must return quickly.
#[tokio::test]
async fn test_thumb_warm() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let tc = env.thumbnail_client();
    ensure_data_flowing(&tc, &env.stream_source).await;

    let t0 = std::time::Instant::now();
    let resp = thumbnail_with_retry(&tc, &env.stream_source).await;
    let elapsed = t0.elapsed();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert!(body.len() > 1000, "thumbnail body too small");
    eprintln!("warm thumbnail: {elapsed:?}, {} bytes", body.len());
}

/// After a thumbnail, the stream must return to Idle.
#[tokio::test]
async fn test_thumb_returns_to_idle() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();
    ensure_idle(&c).await;

    let body = cold_thumbnail(&tc, cold_timeout(), &env.stream_source).await;
    assert!(body.len() > 1000);

    c.wait_for_stream_state(StreamStatusState::Idle, idle_timeout())
        .await
        .expect("stream must return to Idle after thumbnail");
    eprintln!("stream returned to Idle after thumbnail ✓");
}

/// Repeated cold→warm thumbnail cycles. Each time, stream should not go
/// to Stopped and should return to Idle.
#[tokio::test]
async fn test_thumb_cold_warm_cycles() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();

    for cycle in 0..3 {
        ensure_idle(&c).await;
        tokio::time::sleep(Duration::from_secs(5)).await;

        let mon = env.monitor();
        let body = cold_thumbnail(&tc, cold_timeout(), &env.stream_source).await;
        assert!(
            body.len() > 1000,
            "cycle {cycle}: cold thumbnail body too small"
        );

        let resp = thumbnail_with_retry(&tc, &env.stream_source).await;
        assert_eq!(resp.status(), 200, "warm thumbnail cycle {cycle}");

        tokio::time::sleep(Duration::from_secs(1)).await;
        let transitions = mon.stop();
        eprintln!("cycle {cycle}: transitions: {}", states_str(&transitions));
        assert_never_stopped(&transitions, &format!("thumbnail cycle {cycle}"));
    }
}

/// Thumbnail requested repeatedly at 1/s for 10s -- all must succeed,
/// stream must never go Stopped, and must return to Idle after stopping.
#[tokio::test]
async fn test_thumb_rapid_sequential() {
    skip_unless!(SourceTag::Both);
    let env = TestEnv::setup().await;
    let c = env.client();
    let tc = env.thumbnail_client();
    ensure_idle(&c).await;

    let _ = cold_thumbnail(&tc, cold_timeout(), &env.stream_source).await;

    let mon = env.monitor();
    for i in 0..10 {
        let resp = thumbnail_with_retry(&tc, &env.stream_source).await;
        let status = resp.status();
        let body = resp.bytes().await.unwrap();
        eprintln!("thumb #{i}: status={status}, {} bytes", body.len());
        assert_eq!(status, 200, "thumbnail #{i} must return 200");
        assert!(body.len() > 1000, "thumbnail #{i} body too small");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    c.wait_for_stream_state(StreamStatusState::Idle, Duration::from_secs(30))
        .await
        .expect("stream must return to Idle after thumbnail burst");

    let transitions = mon.stop();
    eprintln!(
        "rapid thumbnails: transitions: {}",
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, "rapid thumbnails");
}
