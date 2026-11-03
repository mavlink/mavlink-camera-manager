use super::*;

/// End-to-end assertion for the H.265 counterpart of the JM regression:
/// `x265enc` is Main (`profile-id=1`). When webrtcbin puts that in the
/// offer fmtp, it must be preserved. Some GStreamer versions omit
/// profile-id; then we still reject the old rewrite that forced
/// `level-id=93` and dropped sprop.
#[tokio::test]
async fn test_webrtc_offer_preserves_h265_profile_fields() {
    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_udp_sender(Codec::H265, Some("main"), "127.0.0.1", udp_port);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let producer_name = "redirect_h265_main";
    let redirect = McmClient::build_redirect_udp(producer_name, "127.0.0.1", udp_port);
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create redirect stream: {e}");
    });

    let (bind, available, mut ws_sink, mut ws_stream) =
        start_webrtc_session_for_producer(&mcm.signalling_url(), producer_name, TIMEOUT)
            .await
            .unwrap_or_else(|e| {
                sender.kill().ok();
                panic!("should start WebRTC session on H.265 Main-profile redirect: {e}");
            });

    assert!(
        available.iter().any(|s| s.name.contains(producer_name)),
        "redirect stream should be in available streams: {available:?}"
    );

    assert_eq!(
        available
            .iter()
            .find(|s| s.name.contains(producer_name))
            .unwrap()
            .id,
        bind.producer_id,
        "bind producer_id must match the redirect stream"
    );

    let sdp_text = read_offer_sdp_text(&mut ws_stream).await;

    let fmtp_line = sdp_text
        .lines()
        .find(|l| l.starts_with("a=fmtp:96"))
        .unwrap_or_else(|| panic!("offer must have fmtp for pt 96, SDP:\n{sdp_text}"));

    let (_payload, fmtp_config) = fmtp_line
        .split_once(' ')
        .unwrap_or_else(|| panic!("fmtp must carry payload and config, got: {fmtp_line}"));

    let fmtp_field = |name: &str| -> Option<&str> {
        fmtp_config
            .split(';')
            .find_map(|kv| kv.trim().strip_prefix(&format!("{name}=")))
    };

    // webrtcbin on GStreamer 1.20–1.26 often omits H.265 profile-id in the
    // offer even when x265enc is Main. Require it when present; when absent,
    // still reject the old rewrite that forced level-id=93 and dropped sprop.
    match fmtp_field("profile-id") {
        Some("1") => {}
        Some(other) => panic!(
            "expected H.265 Main profile-id=1 from x265enc profile=main, got {other:?} in SDP:\n{sdp_text}"
        ),
        None => {
            assert!(
                fmtp_field("sprop-vps").is_some()
                    && fmtp_field("sprop-sps").is_some()
                    && fmtp_field("sprop-pps").is_some(),
                "fmtp must carry H.265 sprop parameter sets, got line {fmtp_line:?}\nfull SDP:\n{sdp_text}"
            );
            assert_ne!(
                fmtp_field("level-id"),
                Some("93"),
                "old rewrite forced level-id=93 when profile-id was stripped, SDP:\n{sdp_text}"
            );
        }
    }

    let _ = end_webrtc_session(&mut ws_sink, &bind).await;
    sender.kill().ok();
}
