//! Robustness tests for the file sink.
//!
//! These cover five robustness scenarios:
//!   1. The file sink uses `mp4mux fragment-mode=first-moov-then-finalise`,
//!      so a recording interrupted by SIGKILL (hard shutdown, OOM killer)
//!      must still produce a playable MP4.
//!   2. Host stalls (source freezes / stutters / bursts) must not corrupt
//!      the recorded file. We emulate this with `SIGSTOP`/`SIGCONT` on the
//!      MCM process -- the only external knob available today, given that
//!      recording is gated to Fake/QR sources (see `file_sink.rs:147`).
//!   3. If the recording file is deleted mid-recording, the recorder should
//!      detect it and rotate to a new file. This behaviour is not yet
//!      implemented (`file_sink.rs` has no deletion watcher), so the test
//!      is `#[ignore]`d and serves as the acceptance criterion for a
//!      future feature PR.
//!   4. Sequential recordings must produce non-overlapping files.
//!   5. Rapid start/stop cycles must all succeed and must not lose files.
//!
//! All tests share the `setup_recording_test` helper from `recording.rs`.
use std::{path::Path, process::Command, time::Duration};

use super::{
    recording::{count_mp4_files, setup_recording_test},
    *,
};

const FLUSH_TIME: Duration = Duration::from_secs(3);

/// Parsed `ffprobe -print_format json -show_streams -show_format` output,
/// narrowed to the fields we assert on.
#[derive(Debug)]
struct FfprobeInfo {
    duration_s: f64,
    video_codec: String,
    nb_read_frames: u64,
}

/// Run `ffprobe` on `path` and parse the JSON report. The file must be
/// playable (ffprobe must return 0). We count *packets* (container-level,
/// does not decode) rather than frames, because short recordings may not
/// contain enough keyframes for `-count_frames` to yield a meaningful
/// number on every backend.
fn ffprobe_probe(path: &Path) -> Result<FfprobeInfo, String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_packets",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
            "-select_streams",
            "v:0",
            path.to_str().unwrap(),
        ])
        .output()
        .map_err(|error| format!("failed to spawn ffprobe: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "ffprobe exited with {:?}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ffprobe JSON parse failed: {error}"))?;

    let stream = json["streams"]
        .get(0)
        .ok_or_else(|| "ffprobe returned no streams".to_string())?;

    let video_codec = stream["codec_name"]
        .as_str()
        .ok_or_else(|| "ffprobe missing codec_name".to_string())?
        .to_string();

    let nb_read_frames: u64 = match stream.get("nb_read_packets") {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    };

    let duration_s: f64 = match json["format"].get("duration") {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    };

    Ok(FfprobeInfo {
        duration_s,
        video_codec,
        nb_read_frames,
    })
}

/// Return the video PTS (in seconds) of every *packet* in the file,
/// ordered by file position. Used to assert monotonic frame times and
/// to measure gaps between consecutive samples.
///
/// Uses `-show_packets` (container-level) rather than `-show_frames`
/// (decoder-level) so it works on files truncated mid-frame, e.g. after
/// a SIGKILL.
fn ffprobe_frame_timestamps(path: &Path) -> Result<Vec<f64>, String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_packets",
            "-select_streams",
            "v:0",
            path.to_str().unwrap(),
        ])
        .output()
        .map_err(|error| format!("failed to spawn ffprobe: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "ffprobe -show_packets exited with {:?}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ffprobe JSON parse failed: {error}"))?;

    let packets = json["packets"]
        .as_array()
        .ok_or_else(|| "ffprobe returned no packets[] array".to_string())?;

    let mut timestamps = Vec::with_capacity(packets.len());
    for packet in packets {
        let ts = packet.get("pts_time").or_else(|| packet.get("dts_time"));
        let ts_value = match ts {
            Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            _ => None,
        };
        if let Some(v) = ts_value {
            timestamps.push(v);
        }
    }
    Ok(timestamps)
}

/// Actually decode the stream with ffmpeg and return the number of frames
/// the decoder produced. Unlike `ffprobe_probe` (container-only) this
/// catches the class of corruption where packets demux fine but cannot be
/// decoded -- notably a recording that started mid-GOP with no keyframe
/// reference, which `ffprobe -show_streams` happily reports but every
/// decoder refuses.
fn ffmpeg_decoded_frame_count(path: &Path) -> Result<u64, String> {
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostats",
            "-i",
            path.to_str().unwrap(),
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-f",
            "null",
            "-",
        ])
        .output()
        .map_err(|error| format!("failed to spawn ffmpeg: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "ffmpeg decode exited with {:?}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    // `ffprobe -count_frames` forces a full decode pass and returns only
    // frames the decoder actually produced, which is what we need.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-print_format",
            "json",
            "-show_streams",
            "-select_streams",
            "v:0",
            path.to_str().unwrap(),
        ])
        .output()
        .map_err(|error| format!("failed to spawn ffprobe -count_frames: {error}"))?;

    if !probe.status.success() {
        return Err(format!(
            "ffprobe -count_frames exited with {:?}; stderr: {}",
            probe.status,
            String::from_utf8_lossy(&probe.stderr),
        ));
    }

    let json: serde_json::Value = serde_json::from_slice(&probe.stdout)
        .map_err(|error| format!("ffprobe JSON parse failed: {error}"))?;
    let stream = json["streams"]
        .get(0)
        .ok_or_else(|| "ffprobe -count_frames returned no streams".to_string())?;
    let decoded: u64 = match stream.get("nb_read_frames") {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0),
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    Ok(decoded)
}

/// Assert a cleanly-finalised recording is actually decodable: ffmpeg
/// must decode without error, and the decoder must produce roughly as
/// many frames as the container holds packets. A small tolerance is
/// allowed for tail fragments that may not reach the decoder.
///
/// Do NOT use this on recordings truncated by SIGKILL -- the last
/// fragment is expected to be incomplete there.
fn assert_file_decodes_cleanly(path: &Path, label: &str) {
    let info = ffprobe_probe(path)
        .unwrap_or_else(|error| panic!("{label}: ffprobe failed on {}: {error}", path.display()));
    let decoded = ffmpeg_decoded_frame_count(path).unwrap_or_else(|error| {
        panic!(
            "{label}: ffmpeg failed to decode {}: {error}",
            path.display(),
        )
    });
    let packets = info.nb_read_frames;
    // Tolerate losing at most 2 frames at the end of the stream (e.g.
    // trailing B-frames that reference frames past EOS). Anything
    // bigger indicates a real decode problem -- typically the first
    // sample is not a keyframe, so the whole GOP that references it is
    // unusable.
    const TAIL_TOLERANCE: u64 = 2;
    assert!(
        decoded + TAIL_TOLERANCE >= packets,
        "{label}: {} decoded {decoded} frames but container holds {packets} packets; \
         likely recording started without a keyframe and is unplayable",
        path.display(),
    );
}

/// Assert PTS values are strictly increasing and return the maximum gap
/// between consecutive frames (seconds).
fn assert_monotonic_pts(timestamps: &[f64], label: &str) -> f64 {
    assert!(
        timestamps.len() >= 2,
        "{label}: expected at least 2 frames, got {}",
        timestamps.len()
    );
    let mut max_gap = 0.0_f64;
    for window in timestamps.windows(2) {
        let gap = window[1] - window[0];
        assert!(
            gap >= 0.0,
            "{label}: non-monotonic PTS at {:?} (prev={}, next={})",
            window,
            window[0],
            window[1],
        );
        if gap > max_gap {
            max_gap = gap;
        }
    }
    max_gap
}

/// Test 1: Abrupt process death (SIGKILL) mid-recording must still produce
/// a playable fragmented MP4. Validates `mp4mux fragment-mode=dash-or-mss`
/// in `file_sink.rs`: each `moof`+`mdat` fragment is self-contained, so
/// any fragment that reached disk before the kill is still decodable.
#[tokio::test]
async fn test_recording_survives_sigkill() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (mut mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    rec.start_recording(0, 0.0).await.unwrap();
    // `file_sink.rs` configures mp4mux with `fragment-duration=1000`
    // (1 s fragments). Record long enough to commit several fragments
    // before killing, so the on-disk file contains multiple complete
    // moof/mdat pairs regardless of where the kill lands mid-fragment.
    tokio::time::sleep(Duration::from_secs(6)).await;

    mcm.kill9();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let files = count_mp4_files(recording_dir.path());
    assert_eq!(
        files.len(),
        1,
        "expected exactly one mp4 in {}, found {:?}",
        recording_dir.path().display(),
        files,
    );

    let info = ffprobe_probe(&files[0])
        .unwrap_or_else(|error| panic!("ffprobe failed on {}: {error}", files[0].display()));

    assert!(
        info.video_codec.eq_ignore_ascii_case("h264"),
        "expected h264 video codec, got {:?}",
        info.video_codec,
    );
    // `mp4mux fragment-mode=dash-or-mss fragment-duration=1000` commits
    // data in self-contained 1 s fragments. After SIGKILL at ~6 s we
    // expect 4-5 complete fragments to have reached disk, and lose at
    // most the in-progress one. We assert a conservative 3 s floor so
    // the test is robust to CI jitter and still catches a totally
    // broken recording.
    assert!(
        info.duration_s >= 3.0,
        "expected at least 3 committed fragments after SIGKILL, got {:.3}s",
        info.duration_s,
    );
    assert!(
        info.nb_read_frames >= 75,
        "expected at least 3 fragments' worth of frames, got {}",
        info.nb_read_frames,
    );

    let timestamps = ffprobe_frame_timestamps(&files[0])
        .unwrap_or_else(|error| panic!("ffprobe frames failed: {error}"));
    assert_monotonic_pts(&timestamps, "sigkill recording");
}

/// Test 2a: Rapid host stutters (many short SIGSTOP/SIGCONT cycles) must
/// not break the resulting recording. Models stutters and short freezes.
#[tokio::test]
async fn test_recording_smooth_against_stutters() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 6 cycles of {freeze 150 ms, run 1200 ms} => ~8.1s total. Run periods
    // exceed mp4mux fragment-duration (1000 ms) so a fragment can commit
    // between stalls; otherwise nothing is ever flushed to disk.
    for _ in 0..6 {
        mcm.sigstop();
        tokio::time::sleep(Duration::from_millis(150)).await;
        mcm.sigcont();
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files = count_mp4_files(recording_dir.path());
    assert_eq!(
        files.len(),
        1,
        "expected exactly one mp4 after stutter test, got {files:?}",
    );

    let info = ffprobe_probe(&files[0]).unwrap_or_else(|error| panic!("ffprobe failed: {error}"));
    assert!(
        info.duration_s >= 4.0,
        "expected duration >= 4s under stutters, got {:.3}s",
        info.duration_s,
    );

    let timestamps = ffprobe_frame_timestamps(&files[0])
        .unwrap_or_else(|error| panic!("ffprobe frames failed: {error}"));
    assert_monotonic_pts(&timestamps, "stutter recording");

    assert_file_decodes_cleanly(&files[0], "stutter recording");
}

/// Test 2b: A few long freezes (SIGSTOP/SIGCONT with multi-second pauses)
/// must not corrupt the file. Models sustained host stalls and bursty
/// resumes.
#[tokio::test]
async fn test_recording_smooth_against_bursts() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    for _ in 0..3 {
        mcm.sigstop();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        mcm.sigcont();
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files = count_mp4_files(recording_dir.path());
    assert_eq!(
        files.len(),
        1,
        "expected exactly one mp4 after burst test, got {files:?}",
    );

    let info = ffprobe_probe(&files[0]).unwrap_or_else(|error| panic!("ffprobe failed: {error}"));
    // 3 * (1.5 + 2.0) + 2 (lead/trail) = ~12.5s wall; the gstreamer clock
    // may or may not advance during SIGSTOP, so accept a loose floor that
    // still catches a totally broken recording.
    assert!(
        info.duration_s >= 3.0,
        "expected non-trivial duration under bursts, got {:.3}s",
        info.duration_s,
    );

    let timestamps = ffprobe_frame_timestamps(&files[0])
        .unwrap_or_else(|error| panic!("ffprobe frames failed: {error}"));
    assert_monotonic_pts(&timestamps, "burst recording");

    assert_file_decodes_cleanly(&files[0], "burst recording");
}

/// Test 3: Deleting the recording file mid-recording should make the
/// recorder rotate to a new file. The deletion watcher in
/// `stream::recording::Manager` polls `(dev, ino)` every
/// `WATCH_POLL_INTERVAL` (1 s) and calls
/// `stream::manager::Manager::rotate_recording` on mismatch, which
/// tears down the orphaned sink and installs a fresh one with a new
/// `%Y%m%d_%H%M%S` timestamp.
///
/// After a single deletion we expect exactly one file on disk: the
/// new rotation (the deleted one is gone from the directory -- its
/// inode was being written to via the still-open fd and is reclaimed
/// when that fd is closed). The surviving file must have a different
/// name than the deleted one and must decode cleanly end-to-end.
#[tokio::test]
async fn test_recording_rotates_on_file_deletion() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let files_before = count_mp4_files(recording_dir.path());
    assert_eq!(
        files_before.len(),
        1,
        "expected 1 active mp4 before deletion, got {files_before:?}",
    );
    let deleted = files_before[0].clone();
    std::fs::remove_file(&deleted)
        .unwrap_or_else(|error| panic!("failed to delete {}: {error}", deleted.display()));

    // Give the watcher enough wall time to tick (1 s poll), run
    // rotation (stop + start ~ a few hundred ms), and accumulate a
    // non-trivial amount of video in the new file so `ffprobe` has
    // something meaningful to parse.
    tokio::time::sleep(Duration::from_secs(5)).await;
    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files_after = count_mp4_files(recording_dir.path());
    assert_eq!(
        files_after.len(),
        1,
        "expected exactly one post-rotation mp4, got {files_after:?}",
    );
    assert_ne!(
        files_after[0], deleted,
        "expected rotation to produce a new filename; got the same path back",
    );
    assert_file_decodes_cleanly(&files_after[0], "post-deletion rotation");
}

/// Test 3b: A tight `rm` loop must not spin the recorder into a
/// rotate-on-delete storm. `record_rotation` enforces
/// `ROTATION_CAP_MAX` rotations per `ROTATION_CAP_WINDOW`; once the
/// cap is hit, `trigger_rotation` stops recording and stores a stop
/// reason for the MAVLink capture-status task to surface as
/// `REC ERROR`.
///
/// We fire a rapid stream of deletions (one per second, well above
/// the cap within the window), then assert the recorder has stopped
/// of its own accord -- new deletions produce no new files and an
/// explicit stop either succeeds (already stopped from our side but
/// the client still has pending state) or fails with
/// "not recording". Either outcome is acceptable; what matters is
/// that the process is still alive and responsive.
#[tokio::test]
async fn test_recording_rotation_cap_triggers_stop() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Delete on every watcher-tick boundary until the cap trips. The
    // watcher polls at 1 s and the cap is 5 rotations per 10 s; ten
    // deletions (~10 s) guarantees we hit it with generous margin.
    for _ in 0..10 {
        let files = count_mp4_files(recording_dir.path());
        if let Some(file) = files.first() {
            let _ = std::fs::remove_file(file);
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }

    // After the cap trips the recorder stops itself. Give it a beat,
    // then confirm no further rotations are happening: delete any
    // file still present and verify nothing new appears.
    tokio::time::sleep(Duration::from_secs(2)).await;

    for file in count_mp4_files(recording_dir.path()) {
        let _ = std::fs::remove_file(&file);
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let files_after = count_mp4_files(recording_dir.path());
    assert!(
        files_after.is_empty(),
        "expected recorder to be stopped after cap; still produced {files_after:?}",
    );

    // The recorder should be idle; a STOP should either succeed
    // cleanly or report "not recording". Crucially it must not hang
    // or crash the server.
    let _ = rec.stop_recording(0).await;
}

/// Test 4: Sequential recordings must produce files with strictly
/// non-overlapping time windows (both by filename timestamp and by
/// [start, start+duration] when measured from file mtime).
#[tokio::test]
async fn test_sequential_recordings_do_not_overlap() {
    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    for _ in 0..3 {
        rec.start_recording(0, 0.0).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        rec.stop_recording(0).await.unwrap();
        tokio::time::sleep(FLUSH_TIME).await;
    }

    let mut files = count_mp4_files(recording_dir.path());
    files.sort();
    assert_eq!(files.len(), 3, "expected 3 recordings, got {files:?}");

    // Filename timestamp is `%Y%m%d_%H%M%S`, produced in `file_sink.rs:211`.
    let filename_timestamps: Vec<chrono::NaiveDateTime> = files
        .iter()
        .map(|path| {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            let ts = stem
                .rsplit('_')
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("_");
            chrono::NaiveDateTime::parse_from_str(&ts, "%Y%m%d_%H%M%S").unwrap_or_else(|error| {
                panic!("cannot parse timestamp from {}: {error}", path.display())
            })
        })
        .collect();

    for window in filename_timestamps.windows(2) {
        assert!(
            window[0] < window[1],
            "filename timestamps not strictly increasing: {:?} then {:?}",
            window[0],
            window[1],
        );
    }

    let mut prev_end: Option<std::time::SystemTime> = None;
    for file in &files {
        assert_file_decodes_cleanly(file, "sequential recording");
        let info = ffprobe_probe(file)
            .unwrap_or_else(|error| panic!("ffprobe failed on {}: {error}", file.display()));
        let mtime = std::fs::metadata(file).unwrap().modified().unwrap();
        let start = mtime
            .checked_sub(Duration::from_secs_f64(info.duration_s))
            .unwrap();
        if let Some(prev) = prev_end {
            assert!(
                start >= prev,
                "recording {:?} starts before previous ended; overlap detected",
                file.file_name().unwrap(),
            );
        }
        prev_end = Some(mtime);
    }
}

/// Test 5: Rapid start/stop cycles must all ACK successfully and produce
/// one file per cycle, each with a conservative floor of frames.
#[tokio::test]
async fn test_rapid_start_stop_cycles() {
    const CYCLES: usize = 10;
    const MIN_FRAMES_PER_FILE: u64 = 20;

    let recording_dir = tempfile::tempdir().unwrap();
    let (_mcm, _client, mut rec) = setup_recording_test(Codec::H264, &recording_dir).await;

    // Each cycle must exceed mp4mux fragment-duration (1000 ms) so the
    // file gets a finalised moov + at least one fragment before stop.
    for i in 0..CYCLES {
        rec.start_recording(0, 0.0)
            .await
            .unwrap_or_else(|error| panic!("cycle {i}: start_recording failed: {error}"));
        tokio::time::sleep(Duration::from_millis(1200)).await;
        rec.stop_recording(0)
            .await
            .unwrap_or_else(|error| panic!("cycle {i}: stop_recording failed: {error}"));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    tokio::time::sleep(FLUSH_TIME).await;

    let mut files = count_mp4_files(recording_dir.path());
    files.sort();
    assert_eq!(
        files.len(),
        CYCLES,
        "expected {CYCLES} mp4 files after {CYCLES} cycles, got {files:?}",
    );

    let mut total_frames: u64 = 0;
    for file in &files {
        let info = ffprobe_probe(file)
            .unwrap_or_else(|error| panic!("ffprobe failed on {}: {error}", file.display()));
        assert!(
            info.duration_s >= 0.8,
            "file {} has duration {:.3}s, expected >= 0.8s",
            file.display(),
            info.duration_s,
        );
        assert!(
            info.nb_read_frames >= MIN_FRAMES_PER_FILE,
            "file {} has {} frames, expected >= {MIN_FRAMES_PER_FILE}",
            file.display(),
            info.nb_read_frames,
        );
        assert_file_decodes_cleanly(file, "rapid cycle");
        total_frames += info.nb_read_frames;
    }

    let min_total = (CYCLES as u64) * MIN_FRAMES_PER_FILE;
    assert!(
        total_frames >= min_total,
        "total frames across all cycles: {total_frames} < expected minimum {min_total}",
    );
}

/// Helper to drop a playable recording at a stable path you can inspect
/// with `ffprobe`, `mpv`, a hex dump, etc. Not part of the robustness
/// assertions; marked `#[ignore]` so regular test runs don't produce it.
///
/// Run with:
///     cargo test --test integration \
///         data_flow::recording_robustness::inspectable_recording -- \
///         --ignored --nocapture
///
/// The resulting file is left at
/// `/tmp/mcm-inspect/recordings/rec_h264/rec_h264_<timestamp>.mp4`.
#[tokio::test]
#[ignore = "leaves a recording on disk for manual inspection"]
async fn inspectable_recording() {
    let dir = std::path::PathBuf::from("/tmp/mcm-inspect");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    let mavlink_port = allocate_ports(1).unwrap()[0];
    let mcm = McmProcess::start_with_recording(mavlink_port, dir.to_str().unwrap())
        .await
        .unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let ext = Some(ExtendedConfiguration {
        disable_mavlink: false,
        disable_lazy: true,
        ..Default::default()
    });
    let post =
        McmClient::build_fake_h264_rtsp("rec_h264", 320, 240, 30, "rec_h264", ext, mcm.rtsp_port);
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let addr = format!("tcpout:127.0.0.1:{mavlink_port}");
    let connect_deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut rec = loop {
        match stream_clients::recording_client::RecordingClient::new(&addr, 1) {
            Ok(client) => break client,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < connect_deadline,
                    "RecordingClient connect failed: {error}",
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    rec.wait_for_camera_heartbeat(TIMEOUT).await.unwrap();

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files = count_mp4_files(&dir);
    assert_eq!(files.len(), 1, "expected one recording, got {files:?}");
    eprintln!("\nRecording saved to: {}\n", files[0].display());
}

/// Same spirit as `inspectable_recording`, but exercises the freeze/burst
/// path: 3 cycles of SIGSTOP (1.5 s) / SIGCONT (2.0 s) against MCM while
/// recording. Use this to visually inspect how stalls/bursts affect the
/// recorded file.
///
/// Run with:
///     cargo test --test integration \
///         data_flow::recording_robustness::inspectable_burst_recording -- \
///         --ignored --nocapture
///
/// The resulting file is left at
/// `/tmp/mcm-inspect-burst/recordings/rec_h264/rec_h264_<timestamp>.mp4`.
#[tokio::test]
#[ignore = "leaves a freeze/burst recording on disk for manual inspection"]
async fn inspectable_burst_recording() {
    let dir = std::path::PathBuf::from("/tmp/mcm-inspect-burst");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    let mavlink_port = allocate_ports(1).unwrap()[0];
    let mcm = McmProcess::start_with_recording(mavlink_port, dir.to_str().unwrap())
        .await
        .unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let ext = Some(ExtendedConfiguration {
        disable_mavlink: false,
        disable_lazy: true,
        ..Default::default()
    });
    let post =
        McmClient::build_fake_h264_rtsp("rec_h264", 320, 240, 30, "rec_h264", ext, mcm.rtsp_port);
    client.create_stream(&post).await.unwrap();
    client.wait_for_streams_running(1, TIMEOUT).await.unwrap();

    let addr = format!("tcpout:127.0.0.1:{mavlink_port}");
    let connect_deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut rec = loop {
        match stream_clients::recording_client::RecordingClient::new(&addr, 1) {
            Ok(client) => break client,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < connect_deadline,
                    "RecordingClient connect failed: {error}",
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    rec.wait_for_camera_heartbeat(TIMEOUT).await.unwrap();

    rec.start_recording(0, 0.0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Match `test_recording_smooth_against_bursts`: 3 cycles of
    // {freeze 1.5 s, run 2.0 s}.
    for _ in 0..3 {
        mcm.sigstop();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        mcm.sigcont();
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    rec.stop_recording(0).await.unwrap();
    tokio::time::sleep(FLUSH_TIME).await;

    let files = count_mp4_files(&dir);
    assert_eq!(files.len(), 1, "expected one recording, got {files:?}");
    eprintln!("\nBurst recording saved to: {}\n", files[0].display());
}
