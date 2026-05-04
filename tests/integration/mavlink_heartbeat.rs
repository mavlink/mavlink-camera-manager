use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::common::{
    api::McmClient,
    mcm::{allocate_udp_ports, McmProcess},
    types::*,
};

const TIMEOUT: Duration = Duration::from_secs(15);

/// Extra time beyond the 5s idle grace period so the watcher loop (100ms tick)
/// has time to observe the idle condition and flip the state.
const IDLE_WAIT: Duration = Duration::from_secs(8);

/// When a lazy stream goes idle, its MAVLink heartbeat task must keep running.
/// This test creates a fake RTSP stream with MAVLink enabled, waits for the
/// stream to transition through Running -> Draining -> Idle, then verifies
/// that HEARTBEAT messages (MAV_TYPE_CAMERA) are still being emitted.
#[tokio::test]
async fn heartbeat_persists_through_idle() {
    let mavlink_port = allocate_udp_ports(1).unwrap()[0];

    let (tx, rx) = mpsc::channel::<()>();

    std::thread::spawn(move || {
        let conn = mavlink::connect::<mavlink::common::MavMessage>(&format!(
            "udpin:127.0.0.1:{mavlink_port}"
        ))
        .expect("Failed to create MAVLink listener");

        loop {
            match conn.recv() {
                Ok((_, mavlink::common::MavMessage::HEARTBEAT(hb)))
                    if hb.mavtype == mavlink::common::MavType::MAV_TYPE_CAMERA =>
                {
                    if tx.send(()).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    });

    let mcm = McmProcess::start_with_options(Some(&format!("udpout:127.0.0.1:{mavlink_port}")))
        .await
        .unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let post = McmClient::build_fake_h264_rtsp(
        "heartbeat_test",
        640,
        480,
        30,
        "heartbeat_test",
        Some(ExtendedConfiguration {
            disable_mavlink: false,
            ..Default::default()
        }),
        mcm.rtsp_port,
    );
    client.create_stream(&post).await.unwrap();

    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    tokio::time::sleep(IDLE_WAIT).await;
    client
        .wait_for_stream_state(StreamStatusState::Idle, TIMEOUT)
        .await
        .unwrap();

    // Drain heartbeats that arrived before idle
    while rx.try_recv().is_ok() {}

    // Count heartbeats over 5 seconds (expect ~5 at 1 Hz)
    let mut heartbeat_count = 0u32;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(()) => heartbeat_count += 1,
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    assert!(
        heartbeat_count >= 3,
        "Expected at least 3 heartbeats during 5s idle period, got {heartbeat_count}"
    );
}
