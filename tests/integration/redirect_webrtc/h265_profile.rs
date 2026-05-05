use super::*;

/// gst-launch-1.0 sender that produces an H.265 Main-profile stream via
/// `x265enc` on 8-bit I420 input. This matches what a typical H.265
/// camera emits and exercises the H.265 branch of `customize_sent_sdp`,
/// where the current code strips every fmtp field except `sprop-*` and
/// hardcodes `level-id=93`.
fn spawn_h265_main_udp_sender(host: &str, port: u16) -> std::process::Child {
    let host_arg = format!("host={host}");
    let port_arg = format!("port={port}");

    std::process::Command::new("gst-launch-1.0")
        .args([
            "videotestsrc",
            "is-live=true",
            "pattern=ball",
            "do-timestamp=true",
            "!",
            "video/x-raw,width=160,height=120,framerate=30/1,format=I420",
            "!",
            "x265enc",
            "tune=zerolatency",
            "speed-preset=ultrafast",
            "bitrate=5000",
            "!",
            "video/x-h265,profile=(string)main",
            "!",
            "h265parse",
            "config-interval=-1",
            "!",
            "video/x-h265,stream-format=byte-stream,alignment=au",
            "!",
            "rtph265pay",
            "aggregate-mode=zero-latency",
            "config-interval=-1",
            "pt=96",
            "!",
            "udpsink",
            host_arg.as_str(),
            port_arg.as_str(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start gst-launch-1.0 H265 UDP sender")
}

/// End-to-end assertion for the H.265 counterpart of the JM regression:
/// `x265enc` emits Main profile (`profile-id=1`, `tier-flag=0`), and the
/// wire SDP offer must carry those fmtp fields through so the browser
/// can decode the stream.
///
/// Guards against the old rewrite that stripped `profile-id`,
/// `profile-space`, `tier-flag`, and forced `level-id=93`.
#[tokio::test]
async fn test_webrtc_offer_preserves_h265_profile_fields() {
    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_h265_main_udp_sender("127.0.0.1", udp_port);

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

    let profile_id = fmtp_config
        .split(';')
        .find_map(|kv| kv.trim().strip_prefix("profile-id="))
        .unwrap_or_else(|| {
            panic!(
                "fmtp must carry H.265 profile-id, got line {fmtp_line:?}\nfull SDP:\n{sdp_text}"
            )
        });

    assert_eq!(
        profile_id, "1",
        "expected H.265 Main profile-id=1 from x265enc profile=main, got {profile_id:?} in SDP:\n{sdp_text}",
    );

    let _ = end_webrtc_session(&mut ws_sink, &bind).await;
    sender.kill().ok();
}
