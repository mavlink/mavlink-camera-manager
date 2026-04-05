mod qr;
mod redirect;
mod rtsp;
mod thumbnail;
mod udp;
mod webrtc;
mod zenoh;

use std::time::Duration;

pub(super) use stream_clients::{Codec, FrameSample};
pub(super) use tokio::sync::mpsc;
pub(super) use url::Url;

pub(super) use crate::common::{
    api::{zenoh_topic, McmClient, StateMonitor},
    mcm::{allocate_ports, allocate_udp_ports, McmProcess},
    types::*,
};

pub(super) const TIMEOUT: Duration = Duration::from_secs(30);

/// Measurement window must exceed the idle grace period (5 s) so we prove
/// the stream kept running continuously under real data flow.
pub(super) const MEASUREMENT_WINDOW: Duration = Duration::from_secs(8);

/// Maximum gap between consecutive frames before we consider data flow stalled.
pub(super) const MAX_FRAME_GAP: Duration = Duration::from_secs(3);

/// Minimum number of frames we expect over the measurement window.
pub(super) const MIN_FRAME_COUNT: usize = 10;

pub(super) const NON_LAZY: ExtendedConfiguration = ExtendedConfiguration {
    thermal: false,
    disable_mavlink: true,
    disable_zenoh: true,
    disable_thumbnails: false,
    disable_lazy: true,
};

/// Drain all pending samples from the channel and return them.
pub(super) fn drain(rx: &mut mpsc::UnboundedReceiver<FrameSample>) -> Vec<FrameSample> {
    let mut samples = Vec::new();
    while let Ok(s) = rx.try_recv() {
        samples.push(s);
    }
    samples
}

/// Collect frames over `duration`, asserting no gap exceeds `max_gap`.
pub(super) async fn collect_frames(
    rx: &mut mpsc::UnboundedReceiver<FrameSample>,
    duration: Duration,
    max_gap: Duration,
) -> Vec<FrameSample> {
    let deadline = tokio::time::Instant::now() + duration;
    let mut all_samples: Vec<FrameSample> = Vec::new();

    drain(rx);

    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let batch = drain(rx);
        all_samples.extend(batch);
    }

    if all_samples.len() >= 2 {
        for window in all_samples.windows(2) {
            let gap = window[1].arrival.duration_since(window[0].arrival);
            assert!(
                gap < max_gap,
                "Data flow stalled: {gap:?} gap between consecutive frames (limit: {max_gap:?})"
            );
        }
    }

    all_samples
}

/// Wait for the first frame to arrive on the channel, or panic after timeout.
pub(super) async fn wait_first_frame(
    rx: &mut mpsc::UnboundedReceiver<FrameSample>,
    timeout: Duration,
    label: &str,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !drain(rx).is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "No {label} frames received within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait for the first frame, collect over the measurement window, and assert
/// frame count meets the minimum.
pub(super) async fn verify_data_flow(rx: &mut mpsc::UnboundedReceiver<FrameSample>, label: &str) {
    wait_first_frame(rx, TIMEOUT, label).await;
    let samples = collect_frames(rx, MEASUREMENT_WINDOW, MAX_FRAME_GAP).await;
    assert!(
        samples.len() >= MIN_FRAME_COUNT,
        "Expected at least {MIN_FRAME_COUNT} {label} frames over {MEASUREMENT_WINDOW:?}, got {}",
        samples.len()
    );
}

pub(super) fn verify_never_idle(
    transitions: &[(std::time::Instant, StreamStatusState)],
    label: &str,
) {
    assert!(
        !transitions
            .iter()
            .any(|(_, st)| *st == StreamStatusState::Idle),
        "{label} stream went Idle during data flow test: {transitions:?}"
    );
}

pub(super) async fn wait_for_rtsp_tcp(url: &str, timeout: Duration) {
    let addr = url
        .trim_start_matches("rtsp://")
        .split('/')
        .next()
        .unwrap_or("127.0.0.1:8554");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "RTSP server at {addr} not accepting TCP within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(super) fn build_fake_udp_non_lazy(
    name: &str,
    width: u32,
    height: u32,
    fps: u32,
    host: &str,
    port: u16,
    codec: Codec,
) -> PostStream {
    let (scheme, encode) = match codec {
        Codec::H264 => ("udp", "H264"),
        Codec::H265 => ("udp265", "H265"),
        Codec::Mjpg => ("udp", "MJPG"),
        Codec::Yuyv => ("udp", "YUYV"),
        Codec::Rgb => ("udp", "RGB"),
    };
    PostStream {
        name: name.to_string(),
        source: "ball".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse(&format!("{scheme}://{host}:{port}")).unwrap()],
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode: serde_json::Value::String(encode.to_string()),
                height,
                width,
                frame_interval: FrameInterval {
                    numerator: 1,
                    denominator: fps,
                },
            }),
            extended_configuration: Some(NON_LAZY),
        },
    }
}

pub(super) fn build_fake_rtsp(
    codec: Codec,
    name: &str,
    width: u32,
    height: u32,
    fps: u32,
    path: &str,
    ext: Option<ExtendedConfiguration>,
    rtsp_port: u16,
) -> PostStream {
    match codec {
        Codec::H264 => {
            McmClient::build_fake_h264_rtsp(name, width, height, fps, path, ext, rtsp_port)
        }
        Codec::H265 => {
            McmClient::build_fake_h265_rtsp(name, width, height, fps, path, ext, rtsp_port)
        }
        Codec::Mjpg => {
            McmClient::build_fake_mjpg_rtsp(name, width, height, fps, path, ext, rtsp_port)
        }
        Codec::Yuyv => {
            McmClient::build_fake_yuyv_rtsp(name, width, height, fps, path, ext, rtsp_port)
        }
        Codec::Rgb => {
            panic!("Fake pipeline does not support RGB; use QR pipeline instead")
        }
    }
}

/// Spawn an external gst-launch-1.0 process that sends RTP to host:port.
/// When `profile` is `None` the encoder's default profile is used.
pub(super) fn spawn_udp_sender(
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

    let mut args: Vec<&str> = vec![
        "videotestsrc",
        "is-live=true",
        "pattern=ball",
        "do-timestamp=true",
        "!",
        "video/x-raw,width=160,height=120,framerate=30/1",
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

/// Poll `GET /thumbnail?source=...` until it returns 200 with a non-empty
/// body, or time out.
pub(super) async fn wait_for_thumbnail(
    client: &McmClient,
    source: &str,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(resp) = client.thumbnail(source).await {
            if resp.status().is_success() {
                let bytes = resp.bytes().await.unwrap_or_default();
                if !bytes.is_empty() {
                    return bytes.to_vec();
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "thumbnail for {source:?} not available after {}s",
            timeout.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(super) struct TestRtspServer {
    _server: gst_rtsp_server::RTSPServer,
    _source_id: gst::glib::SourceId,
    pub port: u16,
}

impl TestRtspServer {
    /// Start an RTSP test server producing the given codec and optional
    /// profile. When `profile` is `None` the encoder's default is used.
    pub fn start(codec: Codec, profile: Option<&str>, port: u16, path: &str) -> Self {
        let launch = match codec {
            Codec::H264 => {
                let profile_filter = match profile {
                    Some(p) => format!(" ! video/x-h264,profile=(string){p}"),
                    None => String::new(),
                };
                format!(
                    concat!(
                        "( videotestsrc is-live=true pattern=ball do-timestamp=true",
                        " ! video/x-raw,width=160,height=120,framerate=30/1",
                        " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                        "{profile_filter}",
                        " ! h264parse config-interval=-1",
                        " ! rtph264pay name=pay0 pt=96 )",
                    ),
                    profile_filter = profile_filter,
                )
            }
            Codec::H265 => {
                if profile == Some("main-10") {
                    concat!(
                        "( videotestsrc is-live=true pattern=ball do-timestamp=true",
                        " ! video/x-raw,width=160,height=120,framerate=30/1",
                        " ! videoconvert",
                        " ! video/x-raw,format=I420_10LE,width=160,height=120,framerate=30/1",
                        " ! x265enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                        " ! h265parse config-interval=-1",
                        " ! rtph265pay name=pay0 pt=96 )",
                    )
                    .to_string()
                } else {
                    concat!(
                        "( videotestsrc is-live=true pattern=ball do-timestamp=true",
                        " ! video/x-raw,width=160,height=120,framerate=30/1",
                        " ! x265enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                        " ! h265parse config-interval=-1",
                        " ! rtph265pay name=pay0 pt=96 )",
                    )
                    .to_string()
                }
            }
            other => panic!("TestRtspServer does not support {other:?}"),
        };
        Self::start_with_launch(&launch, port, path)
    }

    fn start_with_launch(launch: &str, port: u16, path: &str) -> Self {
        use gst_rtsp_server::prelude::*;

        ensure_glib_main_loop();

        let server = gst_rtsp_server::RTSPServer::new();
        server.set_service(&port.to_string());

        let factory = gst_rtsp_server::RTSPMediaFactory::new();
        factory.set_launch(launch);
        factory.set_shared(true);

        let mounts = server.mount_points().unwrap();
        mounts.add_factory(&format!("/{path}"), factory);

        let source_id = server.attach(None).unwrap();

        Self {
            _server: server,
            _source_id: source_id,
            port,
        }
    }

    pub fn rtsp_url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("rtsp://127.0.0.1:{}/{path}", self.port)
    }
}

static GLIB_MAIN_LOOP: std::sync::Once = std::sync::Once::new();

fn ensure_glib_main_loop() {
    GLIB_MAIN_LOOP.call_once(|| {
        std::thread::spawn(|| {
            let ctx = gst::glib::MainContext::default();
            loop {
                ctx.iteration(true);
            }
        });
    });
}
