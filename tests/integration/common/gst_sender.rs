use stream_clients::Codec;

/// Spawn an external gst-launch-1.0 process that sends RTP to host:port.
/// When `profile` is `None` the encoder's default profile is used.
pub fn spawn_udp_sender(
    codec: Codec,
    profile: Option<&str>,
    host: &str,
    port: u16,
) -> std::process::Child {
    match codec {
        Codec::H264 => spawn_h264_udp_sender(profile, host, port),
        Codec::H265 => spawn_h265_udp_sender(profile, host, port),
        other => panic!("spawn_udp_sender does not support {other:?}"),
    }
}

fn spawn_h264_udp_sender(profile: Option<&str>, host: &str, port: u16) -> std::process::Child {
    let profile_caps = profile.map(|p| format!("video/x-h264,profile=(string){p}"));
    let host_arg = format!("host={host}");
    let port_arg = format!("port={port}");

    let mut args: Vec<&str> = vec![
        "videotestsrc",
        "is-live=true",
        "pattern=ball",
        "do-timestamp=true",
        "!",
        "video/x-raw,width=160,height=120,framerate=30/1",
        "!",
        "x264enc",
        "tune=zerolatency",
        "speed-preset=ultrafast",
        "bitrate=5000",
    ];
    if let Some(ref caps) = profile_caps {
        args.extend(["!", caps.as_str()]);
    }
    args.extend([
        "!",
        "h264parse",
        "config-interval=-1",
        "!",
        "video/x-h264,stream-format=avc,alignment=au",
        "!",
        "rtph264pay",
        "aggregate-mode=zero-latency",
        "config-interval=-1",
        "pt=96",
        "!",
        "udpsink",
        host_arg.as_str(),
        port_arg.as_str(),
    ]);

    std::process::Command::new("gst-launch-1.0")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start gst-launch-1.0 H264 UDP sender")
}

fn spawn_h265_udp_sender(profile: Option<&str>, host: &str, port: u16) -> std::process::Child {
    let host_arg = format!("host={host}");
    let port_arg = format!("port={port}");
    let raw_caps = if profile == Some("main") {
        "video/x-raw,width=160,height=120,framerate=30/1,format=I420"
    } else {
        "video/x-raw,width=160,height=120,framerate=30/1"
    };

    let mut args: Vec<&str> = vec![
        "videotestsrc",
        "is-live=true",
        "pattern=ball",
        "do-timestamp=true",
        "!",
        raw_caps,
    ];
    if profile == Some("main-10") {
        args.extend([
            "!",
            "videoconvert",
            "!",
            "video/x-raw,format=I420_10LE,width=160,height=120,framerate=30/1",
        ]);
    }
    args.extend([
        "!",
        "x265enc",
        "tune=zerolatency",
        "speed-preset=ultrafast",
        "bitrate=5000",
    ]);
    if profile == Some("main") {
        args.extend(["!", "video/x-h265,profile=(string)main"]);
    }
    args.extend([
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
    ]);

    std::process::Command::new("gst-launch-1.0")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start gst-launch-1.0 H265 UDP sender")
}
