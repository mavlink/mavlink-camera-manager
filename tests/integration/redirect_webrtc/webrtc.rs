use super::*;

/// The redirect stream must appear in the signalling server's available
/// streams list and a WebRTC session must be creatable against it.
/// The server must send an SDP offer containing an H264 media line.
#[tokio::test]
async fn test_redirect_webrtc_session_and_sdp_offer() {
    let (mcm, _client, mut sender) = setup_udp_redirect().await;

    // Start a WebRTC session targeting the redirect producer.
    // The redirect's encode takes time to resolve (probe + brute-force),
    // so poll until it appears in the signalling server's available list.
    let (bind, available, mut ws_sink, mut ws_stream) =
        start_webrtc_session_for_producer(&mcm.signalling_url(), "redirect_receiver", TIMEOUT)
            .await
            .expect("should start WebRTC session on redirect producer");

    // The redirect stream must be in the available list
    assert!(
        available
            .iter()
            .any(|s| s.name.contains("redirect_receiver")),
        "redirect stream should be in available streams: {available:?}"
    );

    // The bind answer must reference the same producer
    assert_eq!(
        available
            .iter()
            .find(|s| s.name.contains("redirect_receiver"))
            .unwrap()
            .id,
        bind.producer_id,
        "bind producer_id must match the redirect stream"
    );

    // Wait for an SDP offer (Negotiation message) from the server.
    // The server sends the offer asynchronously after session creation.
    use futures::StreamExt;
    let sdp_offer = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            let text = match msg.into_text() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let proto: Result<SignallingProtocol, _> = serde_json::from_str(&text);
            let Ok(proto) = proto else { continue };
            if let SignallingMessage::Negotiation(ref val) = proto.message {
                return Some(val.clone());
            }
        }
        None
    })
    .await;

    let sdp_json = sdp_offer
        .expect("should receive negotiation message within 10s")
        .expect("ws stream should not close before SDP offer");

    // The negotiation message should contain an SDP offer with H264
    let sdp_text = sdp_json
        .pointer("/content/sdp/sdp")
        .and_then(|v| v.as_str())
        .expect("negotiation message should contain /content/sdp/sdp field");

    assert!(
        sdp_text.contains("H264") || sdp_text.contains("h264"),
        "SDP offer should mention H264, got:\n{sdp_text}"
    );

    // Clean up
    let _ = end_webrtc_session(&mut ws_sink, &bind).await;
    sender.kill().ok();
}

#[tokio::test]
async fn test_rtsp_redirect_webrtc_session_and_sdp_offer() {
    let (mcm, _client) = setup_fake_rtsp_and_redirect("test_redir").await;

    let (bind, available, mut ws_sink, mut ws_stream) =
        start_webrtc_session_for_producer(&mcm.signalling_url(), "redirect_receiver", TIMEOUT)
            .await
            .expect("should start WebRTC session on RTSP redirect producer");

    assert!(
        available
            .iter()
            .any(|s| s.name.contains("redirect_receiver")),
        "RTSP redirect stream should be in available streams: {available:?}"
    );

    assert_eq!(
        available
            .iter()
            .find(|s| s.name.contains("redirect_receiver"))
            .unwrap()
            .id,
        bind.producer_id,
        "bind producer_id must match the RTSP redirect stream"
    );

    use futures::StreamExt;
    let sdp_offer = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            let text = match msg.into_text() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let proto: Result<SignallingProtocol, _> = serde_json::from_str(&text);
            let Ok(proto) = proto else { continue };
            if let SignallingMessage::Negotiation(ref val) = proto.message {
                return Some(val.clone());
            }
        }
        None
    })
    .await;

    let sdp_json = sdp_offer
        .expect("should receive negotiation message within 10s")
        .expect("ws stream should not close before SDP offer");

    let sdp_text = sdp_json
        .pointer("/content/sdp/sdp")
        .and_then(|v| v.as_str())
        .expect("negotiation message should contain /content/sdp/sdp field");

    assert!(
        sdp_text.contains("H264") || sdp_text.contains("h264"),
        "SDP offer should mention H264, got:\n{sdp_text}"
    );

    let _ = end_webrtc_session(&mut ws_sink, &bind).await;
}

#[tokio::test]
async fn test_h265_redirect_webrtc_session_and_sdp_offer() {
    let (mcm, _client, mut sender) = setup_h265_udp_redirect().await;

    let (bind, available, mut ws_sink, mut ws_stream) =
        start_webrtc_session_for_producer(&mcm.signalling_url(), "redirect_receiver", TIMEOUT)
            .await
            .expect("should start WebRTC session on H265 redirect producer");

    assert!(
        available
            .iter()
            .any(|s| s.name.contains("redirect_receiver")),
        "H265 redirect stream should be in available streams: {available:?}"
    );

    assert_eq!(
        available
            .iter()
            .find(|s| s.name.contains("redirect_receiver"))
            .unwrap()
            .id,
        bind.producer_id,
        "bind producer_id must match the H265 redirect stream"
    );

    use futures::StreamExt;
    let sdp_offer = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            let text = match msg.into_text() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let proto: Result<SignallingProtocol, _> = serde_json::from_str(&text);
            let Ok(proto) = proto else { continue };
            if let SignallingMessage::Negotiation(ref val) = proto.message {
                return Some(val.clone());
            }
        }
        None
    })
    .await;

    let sdp_json = sdp_offer
        .expect("should receive negotiation message within 10s")
        .expect("ws stream should not close before SDP offer");

    let sdp_text = sdp_json
        .pointer("/content/sdp/sdp")
        .and_then(|v| v.as_str())
        .expect("negotiation message should contain /content/sdp/sdp field");

    assert!(
        sdp_text.contains("H265") || sdp_text.contains("h265"),
        "SDP offer should mention H265, got:\n{sdp_text}"
    );

    let _ = end_webrtc_session(&mut ws_sink, &bind).await;
    sender.kill().ok();
}

#[tokio::test]
async fn test_h265_rtsp_redirect_webrtc_session_and_sdp_offer() {
    let (mcm, _client) = setup_fake_h265_rtsp_and_redirect("test_h265_redir").await;

    let (bind, available, mut ws_sink, mut ws_stream) =
        start_webrtc_session_for_producer(&mcm.signalling_url(), "redirect_receiver", TIMEOUT)
            .await
            .expect("should start WebRTC session on H265 RTSP redirect producer");

    assert!(
        available
            .iter()
            .any(|s| s.name.contains("redirect_receiver")),
        "H265 RTSP redirect stream should be in available streams: {available:?}"
    );

    assert_eq!(
        available
            .iter()
            .find(|s| s.name.contains("redirect_receiver"))
            .unwrap()
            .id,
        bind.producer_id,
        "bind producer_id must match the H265 RTSP redirect stream"
    );

    use futures::StreamExt;
    let sdp_offer = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws_stream.next().await {
            let text = match msg.into_text() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let proto: Result<SignallingProtocol, _> = serde_json::from_str(&text);
            let Ok(proto) = proto else { continue };
            if let SignallingMessage::Negotiation(ref val) = proto.message {
                return Some(val.clone());
            }
        }
        None
    })
    .await;

    let sdp_json = sdp_offer
        .expect("should receive negotiation message within 10s")
        .expect("ws stream should not close before SDP offer");

    let sdp_text = sdp_json
        .pointer("/content/sdp/sdp")
        .and_then(|v| v.as_str())
        .expect("negotiation message should contain /content/sdp/sdp field");

    assert!(
        sdp_text.contains("H265") || sdp_text.contains("h265"),
        "SDP offer should mention H265, got:\n{sdp_text}"
    );

    let _ = end_webrtc_session(&mut ws_sink, &bind).await;
}
