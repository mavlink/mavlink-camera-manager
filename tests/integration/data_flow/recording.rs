use std::time::Duration;

use stream_clients::recording_client::RecordingClient;

use super::*;

const RECORDING_DURATION: Duration = Duration::from_secs(5);
const FLUSH_TIME: Duration = Duration::from_secs(3);

/// Start MCM with MAVLink + recording, create a stream, and return the
/// connected RecordingClient ready for commands. Each test owns the
/// `recording_dir` so parallel tests never collide.
pub(super) async fn setup_recording_test(
    codec: Codec,
    recording_dir: &tempfile::TempDir,
) -> (McmProcess, McmClient, RecordingClient) {
    let mavlink_port = allocate_ports(1).unwrap()[0];
    let mcm =
        McmProcess::start_with_recording(mavlink_port, recording_dir.path().to_str().unwrap())
            .await
            .unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let ext = Some(ExtendedConfiguration {
        disable_mavlink: false,
        disable_lazy: true,
        ..Default::default()
    });

    let post = match codec {
        Codec::H264 => McmClient::build_fake_h264_rtsp(
            "rec_h264",
            320,
            240,
            30,
            "rec_h264",
            ext,
            mcm.rtsp_port,
        ),
        Codec::H265 => McmClient::build_fake_h265_rtsp(
            "rec_h265",
            320,
            240,
            30,
            "rec_h265",
            ext,
            mcm.rtsp_port,
        ),
        _ => panic!("Recording tests only support H264/H265"),
    };

    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let addr = format!("tcpout:127.0.0.1:{mavlink_port}");
    // MCM's mavlink receiver loop sleeps 1s before binding the TCP listener
    // (see src/lib/mavlink/manager.rs:234), so the connect here races the
    // listener. Poll until the TCP connection succeeds.
    let connect_deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut rec = loop {
        match RecordingClient::new(&addr, 1) {
            Ok(client) => break client,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < connect_deadline,
                    "RecordingClient failed to connect to {addr} within {TIMEOUT:?}: {error}",
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    rec.wait_for_camera_heartbeat(TIMEOUT).await.unwrap();

    (mcm, client, rec)
}

pub(super) fn count_mp4_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if !dir.exists() {
        return files;
    }
    for entry in walkdir(dir) {
        if entry.extension().is_some_and(|e| e == "mp4") {
            files.push(entry);
        }
    }
    files
}

pub(super) fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(walkdir(&path));
            } else {
                result.push(path);
            }
        }
    }
    result
}

async fn run_recording_test(codec: Codec) {
    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(codec, &recording_dir).await;

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(RECORDING_DURATION).await;
    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files = count_mp4_files(recording_dir.path());
    assert!(
        !files.is_empty(),
        "Expected .mp4 files in {}, found none",
        recording_dir.path().display(),
    );
}

#[tokio::test]
async fn test_h264_recording_via_mavlink() {
    run_recording_test(Codec::H264).await;
}

#[tokio::test]
async fn test_h265_recording_via_mavlink() {
    run_recording_test(Codec::H265).await;
}

#[tokio::test]
async fn test_camera_information() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    let info = rec
        .request_camera_information(Duration::from_secs(5))
        .await
        .unwrap();

    assert!(
        !info.vendor_name.is_empty(),
        "Expected non-empty vendor_name, got {:?}",
        info
    );
    assert!(
        !info.model_name.is_empty(),
        "Expected non-empty model_name, got {:?}",
        info
    );
    assert!(
        info.resolution_h > 0 && info.resolution_v > 0,
        "Expected non-zero resolution, got {}x{}",
        info.resolution_h,
        info.resolution_v,
    );
}

#[tokio::test]
async fn test_storage_information() {
    use sysinfo::{DiskExt, System, SystemExt};

    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    let storage = rec
        .request_storage_information(Duration::from_secs(5))
        .await
        .unwrap();

    // Cross-check the MAVLink values against what `sysinfo` reports for
    // "/" (the same mount MCM samples in `mavlink::sys_info`). This
    // pins the unit as MiB; a regression to KB would explode the
    // reported values by ~1024×, a regression to GiB would shrink them
    // by the same factor -- both trip the ±10 % tolerance below.
    let mut system = System::new_all();
    system.refresh_disks();
    let root = system
        .disks()
        .iter()
        .find(|disk| disk.mount_point().as_os_str() == "/")
        .expect("test host has no '/' mount");
    const BYTES_PER_MIB: f64 = (1u64 << 20) as f64;
    let expected_total_mb = root.total_space() as f64 / BYTES_PER_MIB;
    let expected_available_mb = root.available_space() as f64 / BYTES_PER_MIB;

    assert!(
        storage.total_capacity_mb > 0.0,
        "Expected total_capacity > 0, got {}",
        storage.total_capacity_mb,
    );
    assert!(
        storage.available_capacity_mb > 0.0,
        "Expected available_capacity > 0, got {}",
        storage.available_capacity_mb,
    );
    // ±10 % tolerance: absorbs fluctuations between MCM's sample and
    // ours (tempdir allocations, kernel caches, other processes on CI)
    // while still catching any unit-scale regression.
    let total_ratio = f64::from(storage.total_capacity_mb) / expected_total_mb;
    let available_ratio = f64::from(storage.available_capacity_mb) / expected_available_mb;
    assert!(
        (0.9..=1.1).contains(&total_ratio),
        "total_capacity_mb={} MiB but host reports {:.0} MiB for '/' (ratio {:.3}); \
         likely a unit regression in mavlink::sys_info",
        storage.total_capacity_mb,
        expected_total_mb,
        total_ratio,
    );
    assert!(
        (0.9..=1.1).contains(&available_ratio),
        "available_capacity_mb={} MiB but host reports {:.0} MiB for '/' (ratio {:.3}); \
         likely a unit regression in mavlink::sys_info",
        storage.available_capacity_mb,
        expected_available_mb,
        available_ratio,
    );
}

#[tokio::test]
async fn test_capture_status_while_recording() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    rec.start_recording(0, 1.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let status1 = rec
        .request_capture_status(Duration::from_secs(5))
        .await
        .unwrap();

    assert!(
        status1.is_recording,
        "Expected is_recording=true, got {:?}",
        status1
    );
    assert!(
        status1.recording_time_ms > 0,
        "Expected recording_time_ms > 0, got {}",
        status1.recording_time_ms
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    let status2 = rec
        .request_capture_status(Duration::from_secs(5))
        .await
        .unwrap();

    assert!(
        status2.recording_time_ms > status1.recording_time_ms,
        "Expected recording_time_ms to increase: first={}, second={}",
        status1.recording_time_ms,
        status2.recording_time_ms,
    );

    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files = count_mp4_files(recording_dir.path());
    assert!(
        !files.is_empty(),
        "Expected .mp4 files in {}",
        recording_dir.path().display(),
    );
}
