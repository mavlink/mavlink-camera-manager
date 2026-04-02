use std::{future::Future, time::Duration};

pub(super) use crate::common::{
    api::{zenoh_topic, McmClient, StateMonitor},
    mcm::McmProcess,
    types::*,
};

pub(super) use stream_clients::rtsp_client::RtspClient;
pub(super) use stream_clients::thumbnail_client::ThumbnailClient;
pub(super) use stream_clients::webrtc_client::WebrtcClient;
pub(super) use stream_clients::{Codec, StreamClient};

pub(super) const STATE_POLL: Duration = Duration::from_millis(200);
pub(super) const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceMode {
    Fake,
    Camera,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum SourceTag {
    Fake,
    Camera,
    Both,
}

pub(super) fn source_mode() -> SourceMode {
    match std::env::var("TEST_SOURCE_MODE").as_deref() {
        Ok("camera") => SourceMode::Camera,
        _ => SourceMode::Fake,
    }
}

pub(super) struct TestEnv {
    _mcm: Option<McmProcess>,
    pub rest_url: String,
    pub signalling_url: String,
    pub signalling_port: u16,
    pub stream_source: String,
    pub mcm_rtsp_url: String,
    pub ice_filter: Option<String>,
}

impl TestEnv {
    pub async fn setup() -> Self {
        match source_mode() {
            SourceMode::Fake => {
                let mcm = McmProcess::start().await.unwrap();
                let client = McmClient::new(&mcm.rest_url());
                let post = McmClient::build_fake_h264_rtsp(
                    "test_stream",
                    640,
                    480,
                    30,
                    "test",
                    None,
                    mcm.rtsp_port,
                );
                client.create_stream(&post).await.unwrap();
                client
                    .wait_for_streams_running(1, SETUP_TIMEOUT)
                    .await
                    .unwrap();

                let rest_url = mcm.rest_url();
                let signalling_url = mcm.signalling_url();
                let signalling_port = mcm.signalling_port;
                let mcm_rtsp_url = mcm.rtsp_url("test");

                Self {
                    _mcm: Some(mcm),
                    rest_url,
                    signalling_url,
                    signalling_port,
                    stream_source: "ball".to_string(),
                    mcm_rtsp_url,
                    ice_filter: None,
                }
            }
            SourceMode::Camera => {
                let mcm_host =
                    std::env::var("TEST_MCM_HOST").unwrap_or_else(|_| "192.168.2.2".into());
                let camera_host =
                    std::env::var("TEST_CAMERA_HOST").unwrap_or_else(|_| "192.168.2.10".into());

                let rest_url =
                    std::env::var("MCM_REST").unwrap_or_else(|_| format!("http://{mcm_host}:6020"));
                let signalling_url = std::env::var("MCM_SIGNALLING")
                    .unwrap_or_else(|_| format!("ws://{mcm_host}:6021"));
                let stream_source = std::env::var("STREAM_SOURCE")
                    .unwrap_or_else(|_| format!("rtsp://{camera_host}:554/stream_0"));
                let mcm_rtsp_url = std::env::var("MCM_RTSP")
                    .unwrap_or_else(|_| format!("rtsp://{mcm_host}:8554/radcam_{camera_host}/0"));

                let ice_prefix = mcm_host
                    .rsplitn(2, '.')
                    .nth(1)
                    .map(|subnet| format!("{subnet}."))
                    .unwrap_or_else(|| mcm_host.clone());

                let signalling_port = signalling_url
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.parse::<u16>().ok())
                    .unwrap_or(6021);

                Self {
                    _mcm: None,
                    rest_url,
                    signalling_url,
                    signalling_port,
                    stream_source,
                    mcm_rtsp_url,
                    ice_filter: Some(ice_prefix),
                }
            }
        }
    }

    pub async fn setup_with_zenoh() -> Self {
        let mcm = McmProcess::start_with_zenoh().await.unwrap();
        let client = McmClient::new(&mcm.rest_url());
        let post = McmClient::build_fake_h264_rtsp(
            "test_stream",
            640,
            480,
            30,
            "test",
            None,
            mcm.rtsp_port,
        );
        client.create_stream(&post).await.unwrap();
        client
            .wait_for_streams_running(1, SETUP_TIMEOUT)
            .await
            .unwrap();

        let rest_url = mcm.rest_url();
        let signalling_url = mcm.signalling_url();
        let signalling_port = mcm.signalling_port;
        let mcm_rtsp_url = mcm.rtsp_url("test");

        Self {
            _mcm: Some(mcm),
            rest_url,
            signalling_url,
            signalling_port,
            stream_source: "ball".to_string(),
            mcm_rtsp_url,
            ice_filter: None,
        }
    }

    pub fn mcm_pid(&self) -> u32 {
        self._mcm
            .as_ref()
            .expect("mcm_pid() requires a local MCM instance")
            .pid()
    }

    pub fn zenoh_config(&self) -> ::zenoh::Config {
        self._mcm
            .as_ref()
            .expect("zenoh_config() requires a local MCM instance")
            .zenoh_config()
    }

    pub fn client(&self) -> McmClient {
        McmClient::new(&self.rest_url)
    }

    pub fn thumbnail_client(&self) -> ThumbnailClient {
        ThumbnailClient::new(&self.rest_url)
    }

    pub fn monitor(&self) -> StateMonitor {
        StateMonitor::start(&self.rest_url, STATE_POLL)
    }
}

/// Returns early from the calling test if the current source mode
/// doesn't match the tag. Usage: `skip_unless!(SourceTag::Both);`
macro_rules! skip_unless {
    ($tag:expr) => {
        match ($tag, source_mode()) {
            (SourceTag::Both, _) => {}
            (SourceTag::Fake, SourceMode::Fake) => {}
            (SourceTag::Camera, SourceMode::Camera) => {}
            (tag, mode) => {
                eprintln!("SKIP: test tagged {tag:?}, current mode is {mode:?}");
                return;
            }
        }
    };
}

pub(super) fn cold_timeout() -> Duration {
    match source_mode() {
        SourceMode::Fake => Duration::from_secs(30),
        SourceMode::Camera => Duration::from_secs(60),
    }
}

pub(super) fn warm_timeout() -> Duration {
    match source_mode() {
        SourceMode::Fake => Duration::from_secs(10),
        SourceMode::Camera => Duration::from_secs(45),
    }
}

pub(super) fn idle_timeout() -> Duration {
    match source_mode() {
        SourceMode::Fake => Duration::from_secs(30),
        SourceMode::Camera => Duration::from_secs(45),
    }
}

pub(super) async fn ensure_idle(c: &McmClient) {
    c.wait_for_stream_state(StreamStatusState::Idle, idle_timeout())
        .await
        .expect("stream must reach Idle");
}

pub(super) async fn ensure_running(c: &McmClient) {
    c.wait_for_stream_state(StreamStatusState::Running, cold_timeout())
        .await
        .expect("stream did not reach Running");
}

pub(super) async fn ensure_data_flowing(tc: &ThumbnailClient, source: &str) {
    tc.ensure_data_flowing(source, cold_timeout()).await;
}

pub(super) async fn cold_thumbnail(
    tc: &ThumbnailClient,
    timeout: Duration,
    source: &str,
) -> Vec<u8> {
    tc.cold_thumbnail(source, timeout).await
}

pub(super) async fn thumbnail_with_retry(tc: &ThumbnailClient, source: &str) -> reqwest::Response {
    tc.thumbnail_with_retry(source, 3).await
}

pub(super) async fn webrtc_connect_with_retry(url: &str, ice_filter: Option<&str>) -> WebrtcClient {
    let mut last_err = None;
    for attempt in 0..3u32 {
        match WebrtcClient::connect(url, None, None, ice_filter).await {
            Ok(client) => return client,
            Err(e) => {
                eprintln!("WebRTC connect attempt {}/3 failed: {e:#}", attempt + 1);
                last_err = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
    panic!(
        "WebRTC connect failed after 3 attempts: {}",
        last_err.unwrap()
    );
}

pub(super) fn states_str(transitions: &[(std::time::Instant, StreamStatusState)]) -> String {
    transitions
        .iter()
        .map(|(_, s)| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(" → ")
}

pub(super) fn assert_never_stopped(
    transitions: &[(std::time::Instant, StreamStatusState)],
    context: &str,
) {
    let stopped = transitions
        .iter()
        .any(|(_, s)| *s == StreamStatusState::Stopped);
    assert!(
        !stopped,
        "{context}: stream went through Stopped! Transitions: {}",
        states_str(transitions)
    );
}

pub(super) async fn scenario_cold_no_stopped(
    c: &McmClient,
    mon: StateMonitor,
    client: &(dyn StreamClient + Sync),
    label: &str,
) {
    ensure_running(c).await;
    let n = client
        .wait_for_frames(5, warm_timeout())
        .await
        .unwrap_or_else(|e| panic!("cold {label} must receive frames: {e}"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let transitions = mon.stop();
    eprintln!(
        "cold {label}: {n} frames, transitions: {}",
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, &format!("cold {label}"));
}

pub(super) async fn scenario_warm_receives_frames(
    client: &(dyn StreamClient + Sync),
    timeout: Duration,
    label: &str,
) {
    let n = client
        .wait_for_frames(5, timeout)
        .await
        .unwrap_or_else(|e| panic!("warm {label} must receive frames: {e}"));
    eprintln!("warm {label} received {n} frames");
}

pub(super) async fn scenario_immediate_reconnect<F, Fut, C>(
    c: &McmClient,
    mon: StateMonitor,
    make_client: F,
    reconnect_delay: Duration,
    label: &str,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = C>,
    C: StreamClient + Sync,
{
    let client = make_client().await;
    ensure_running(c).await;
    client
        .wait_for_frames(5, warm_timeout())
        .await
        .unwrap_or_else(|e| panic!("first {label} must receive frames: {e}"));
    drop(client);
    eprintln!("[{label}-reconnect] first session dropped");

    if !reconnect_delay.is_zero() {
        tokio::time::sleep(reconnect_delay).await;
    }

    let client = make_client().await;
    let n = client
        .wait_for_frames(5, warm_timeout())
        .await
        .unwrap_or_else(|e| panic!("immediate {label} reconnect must receive frames: {e}"));

    let transitions = mon.stop();
    eprintln!(
        "{label} immediate reconnect: {n} frames, transitions: {}",
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, &format!("{label} immediate reconnect"));
}

pub(super) async fn scenario_stays_alive(
    c: &McmClient,
    mon: StateMonitor,
    client: &(dyn StreamClient + Sync),
    label: &str,
) {
    ensure_running(c).await;
    client
        .wait_for_frames(5, warm_timeout())
        .await
        .unwrap_or_else(|e| panic!("{label} must start receiving frames: {e}"));

    let n = client
        .wait_for_continuous_frames(Duration::from_secs(30), Duration::from_millis(500))
        .await
        .unwrap_or_else(|e| panic!("{label} must keep receiving frames for 30s: {e}"));

    let streams = c.list_streams().await.expect("list streams");
    let state = streams.first().map(|s| s.state);
    eprintln!("{label} alive: {n} frames over 30s, current state: {state:?}");
    assert_eq!(
        state,
        Some(StreamStatusState::Running),
        "stream must be Running while {label} client is connected"
    );

    let transitions = mon.stop();
    assert_never_stopped(&transitions, &format!("{label} stays alive"));
}

pub(super) async fn scenario_returns_to_idle<F, Fut, C>(c: &McmClient, make_client: F, label: &str)
where
    F: Fn() -> Fut,
    Fut: Future<Output = C>,
    C: StreamClient + Sync,
{
    let client = make_client().await;
    ensure_running(c).await;
    client
        .wait_for_frames(5, warm_timeout())
        .await
        .unwrap_or_else(|e| panic!("{label} must receive frames: {e}"));
    drop(client);

    c.wait_for_stream_state(StreamStatusState::Idle, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("stream must return to Idle after {label} disconnect: {e}"));
    eprintln!("{label} returns to idle");
}

pub(super) async fn scenario_reconnect_after_idle<F, Fut, C>(
    c: &McmClient,
    rest_url: &str,
    make_client: F,
    label: &str,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = C>,
    C: StreamClient + Sync,
{
    let client = make_client().await;
    ensure_running(c).await;
    client
        .wait_for_frames(5, warm_timeout())
        .await
        .unwrap_or_else(|e| panic!("first {label} session must get frames: {e}"));
    drop(client);

    ensure_idle(c).await;
    eprintln!("[{label}-after-idle] confirmed idle");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let mon = StateMonitor::start(rest_url, STATE_POLL);
    let client = make_client().await;
    ensure_running(c).await;
    let n = client
        .wait_for_frames(5, cold_timeout())
        .await
        .unwrap_or_else(|e| panic!("{label} reconnect after idle must receive frames: {e}"));

    let transitions = mon.stop();
    eprintln!(
        "{label} reconnect after idle: {n} frames, transitions: {}",
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, &format!("{label} reconnect after idle"));
}

pub(super) async fn scenario_rapid_cycles<F, Fut, C>(
    c: &McmClient,
    mon: StateMonitor,
    make_client: F,
    cycles: u32,
    label: &str,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = C>,
    C: StreamClient + Sync,
{
    for cycle in 0..cycles {
        ensure_idle(c).await;
        let client = make_client().await;
        client
            .wait_for_frames(3, warm_timeout())
            .await
            .unwrap_or_else(|e| panic!("{label} cycle {cycle}: {e}"));
        drop(client);
        eprintln!("{label} cycle {cycle} ok");
    }

    let transitions = mon.stop();
    eprintln!(
        "{label} rapid cycles: transitions: {}",
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, &format!("{label} rapid cycles"));
}

/// Like `scenario_rapid_cycles` but lets each connection settle for 5 s
/// before disconnecting and logs per-cycle frame counts.
pub(super) async fn scenario_extended_cycles<F, Fut, C>(
    c: &McmClient,
    mon: StateMonitor,
    make_client: F,
    cycles: u32,
    label: &str,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = C>,
    C: StreamClient + Sync,
{
    for cycle in 0..cycles {
        ensure_idle(c).await;

        let client = make_client().await;
        client
            .wait_for_frames(5, warm_timeout())
            .await
            .unwrap_or_else(|e| panic!("{label} cycle {cycle}: {e}"));

        tokio::time::sleep(Duration::from_secs(5)).await;

        let frames = client.frames();
        drop(client);
        eprintln!("{label} cycle {cycle} ok -- {frames} frames");
    }

    let transitions = mon.stop();
    eprintln!(
        "{label} extended cycles: transitions: {}",
        states_str(&transitions)
    );
    assert_never_stopped(&transitions, &format!("{label} extended cycles"));
}

/// Keep an anchor client connected (pipeline stays warm) while cycling
/// a second client type on/off. After all cycles, verify the anchor
/// still receives frames.
pub(super) async fn scenario_multi_warm_sequential<F, Fut, C>(
    anchor: &(dyn StreamClient + Sync),
    make_client: F,
    cycles: u32,
    anchor_label: &str,
    cycle_label: &str,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = C>,
    C: StreamClient + Sync,
{
    for cycle in 0..cycles {
        let client = make_client().await;
        client
            .wait_for_frames(5, warm_timeout())
            .await
            .unwrap_or_else(|e| panic!("warm {cycle_label} cycle {cycle}: {e}"));

        tokio::time::sleep(Duration::from_secs(5)).await;

        let frames = client.frames();
        eprintln!("warm {cycle_label} cycle {cycle} ok -- {frames} frames");
        drop(client);

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let before = anchor.frames();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after = anchor.frames();
    assert!(
        after > before,
        "{anchor_label} must still flow after {cycle_label} cycles \
         (before={before}, after={after})"
    );
}

mod mixed;
mod rtsp;
mod thread_leak;
mod thumb;
mod webrtc;
mod zenoh;
