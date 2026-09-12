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
    api::{McmClient, StateMonitor, zenoh_topic},
    gst_sender::spawn_udp_sender,
    mcm::{McmProcess, allocate_ports, allocate_udp_ports},
    poll::{drain, wait_first_frame, wait_for_rtsp_tcp, wait_for_thumbnail},
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
