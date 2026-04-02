use super::*;

#[tokio::test]
async fn test_webrtc_cold_session() {
    let (mcm, client) = setup_fake_rtsp("wrtc_cold", "wrtc_cold").await;

    wait_for_idle(&client).await;

    let (bind, mut sink, _stream) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

    // Stream should be Running now
    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "WebRTC session should wake the pipeline"
    );

    end_webrtc_session(&mut sink, &bind).await.unwrap();
}

#[tokio::test]
async fn test_webrtc_warm_session() {
    let (mcm, client) = setup_fake_rtsp("wrtc_warm", "wrtc_warm").await;

    let (bind, mut sink, _stream) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

    let streams = client.list_streams().await.unwrap();
    assert_eq!(streams[0].state, StreamStatusState::Running);

    end_webrtc_session(&mut sink, &bind).await.unwrap();
}

#[tokio::test]
async fn test_webrtc_multiple_start_stop() {
    let (mcm, client) = setup_fake_rtsp("wrtc_startstop", "wrtc_startstop").await;

    for _cycle in 0..3 {
        wait_for_idle(&client).await;

        let (bind, mut sink, _stream) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

        let streams = client.list_streams().await.unwrap();
        assert_eq!(streams[0].state, StreamStatusState::Running);

        end_webrtc_session(&mut sink, &bind).await.unwrap();
    }
}

#[tokio::test]
async fn test_webrtc_multiple_clients() {
    let (mcm, client) = setup_fake_rtsp("wrtc_multi", "wrtc_multi").await;

    let mut sessions = Vec::new();
    for _ in 0..3 {
        let (bind, sink, stream) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();
        sessions.push((bind, sink, stream));
    }

    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "multiple WebRTC clients should keep pipeline running"
    );

    for (bind, ref mut sink, _) in &mut sessions {
        end_webrtc_session(sink, bind).await.unwrap();
    }
}

#[tokio::test]
async fn test_webrtc_disconnect_one_doesnt_affect_others() {
    let (mcm, client) = setup_fake_rtsp("wrtc_indep", "wrtc_indep").await;

    let (bind1, mut sink1, _s1) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();
    let (bind2, mut sink2, _s2) = start_webrtc_session(&mcm.signalling_url()).await.unwrap();

    // Disconnect client 1
    end_webrtc_session(&mut sink1, &bind1).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Stream should still be Running because client 2 is active
    let streams = client.list_streams().await.unwrap();
    assert_eq!(
        streams[0].state,
        StreamStatusState::Running,
        "disconnecting one WebRTC client should not affect others"
    );

    end_webrtc_session(&mut sink2, &bind2).await.unwrap();
}
