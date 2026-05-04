use std::{
    collections::{HashMap, VecDeque},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use sysinfo::{DiskExt, System, SystemExt};
use tokio::sync::{oneshot, RwLock};
use tracing::*;

use crate::{
    cli,
    stream::sink::{create_file_sink, Sink},
    video_stream::types::VideoAndStreamInformation,
};

lazy_static::lazy_static! {
    static ref MANAGER: Arc<RwLock<Manager>> = Arc::new(RwLock::new(Manager::new()));
}

const MAX_START_ATTEMPTS: u32 = 10;
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// How often the deletion watcher restats the output file.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long the watcher waits for `filesink` to actually create the file
/// before declaring the pipeline dead and bailing. Covers slow CI starts
/// and pipelines that never reach PLAYING.
const WATCH_INITIAL_WAIT: Duration = Duration::from_secs(10);

/// Sliding window used to rate-limit rotations on deletion.
const ROTATION_CAP_WINDOW: Duration = Duration::from_secs(10);

/// Maximum rotations allowed within `ROTATION_CAP_WINDOW` before we stop
/// recording entirely. Protects against a runaway `rm` loop that would
/// otherwise spin MCM into a tight rotate-on-delete storm.
const ROTATION_CAP_MAX: usize = 5;

pub struct Manager {
    config: RecordingConfig,
    active_recordings: HashMap<uuid::Uuid, ActiveRecording>,
    /// Timestamps of past rotations, keyed by stream id. Preserved across
    /// the internal stop/start of a rotation, cleared on a user-initiated
    /// stop. Used by `record_rotation` to enforce `ROTATION_CAP_MAX`.
    rotation_history: HashMap<uuid::Uuid, VecDeque<Instant>>,
    /// Error reason attached when recording stops unexpectedly (rotation
    /// cap exceeded, etc.). Consumed once via `take_stop_reason`, which
    /// the MAVLink capture-status task uses to emit a `REC ERROR`
    /// STATUSTEXT on behalf of the client.
    stop_reasons: HashMap<uuid::Uuid, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordingConfig {
    /// Base path for recordings
    pub path: String,
    /// Minimum percentage of disk space that must remain free (0–100)
    pub min_free_disk_percent: u8,
}

struct ActiveRecording {
    sink_id: uuid::Uuid,
    start_instant: Instant,
    file_path: String,
    /// Dropping this sender wakes the deletion watcher's `select!` and
    /// causes it to exit. Taken on stop or on stream removal.
    cancel_tx: Option<oneshot::Sender<()>>,
}

impl Manager {
    fn new() -> Self {
        Self {
            config: RecordingConfig {
                path: cli::manager::recording_path(),
                min_free_disk_percent: cli::manager::min_free_disk_percent(),
            },
            active_recordings: HashMap::new(),
            rotation_history: HashMap::new(),
            stop_reasons: HashMap::new(),
        }
    }

    pub async fn is_recording(stream_id: &uuid::Uuid) -> bool {
        MANAGER
            .read()
            .await
            .active_recordings
            .contains_key(stream_id)
    }

    pub async fn get_recording_duration_ms(stream_id: &uuid::Uuid) -> u64 {
        MANAGER
            .read()
            .await
            .active_recordings
            .get(stream_id)
            .map(|r| r.start_instant.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    /// Get the file path of the active recording for a stream.
    pub async fn get_recording_file_path(stream_id: &uuid::Uuid) -> Option<String> {
        MANAGER
            .read()
            .await
            .active_recordings
            .get(stream_id)
            .map(|r| r.file_path.clone())
    }

    /// Start recording for a stream, retrying up to MAX_START_ATTEMPTS times.
    /// Returns the Sink to be added to the pipeline.
    #[instrument(level = "debug", skip(video_and_stream_information))]
    pub async fn start_recording(
        stream_id: uuid::Uuid,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Sink> {
        {
            let manager = MANAGER.read().await;
            if manager.active_recordings.contains_key(&stream_id) {
                return Err(anyhow!("Stream {stream_id} is already recording"));
            }
        }

        let mut last_error = None;
        for attempt in 1..=MAX_START_ATTEMPTS {
            match Self::try_start(stream_id, video_and_stream_information).await {
                Ok(sink) => return Ok(sink),
                Err(error) => {
                    warn!(
                        "Recording start attempt {attempt}/{MAX_START_ATTEMPTS} failed: {error:#}"
                    );
                    last_error = Some(error);
                    if attempt < MAX_START_ATTEMPTS {
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("Failed to start recording")))
    }

    async fn try_start(
        stream_id: uuid::Uuid,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Sink> {
        let mut manager = MANAGER.write().await;

        check_disk_space(&manager.config)?;

        let path = manager.config.path.clone();
        let sink_id = Arc::new(uuid::Uuid::new_v4());
        let file_sink =
            create_file_sink(sink_id.clone(), video_and_stream_information, Some(path))?;

        let Sink::File(fs) = &file_sink else {
            unreachable!("create_file_sink always returns Sink::File")
        };
        let file_path = fs.file_path().to_owned();
        let stream_name = video_and_stream_information.name.clone();

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

        manager.active_recordings.insert(
            stream_id,
            ActiveRecording {
                sink_id: *sink_id,
                start_instant: Instant::now(),
                file_path: file_path.clone(),
                cancel_tx: Some(cancel_tx),
            },
        );

        // Spawn the deletion watcher as a detached task. It sleeps on
        // `WATCH_POLL_INTERVAL` and trips rotation when its baseline
        // `(dev, ino)` either disappears (`rm`) or changes (`mv X over`
        // our path). Cancelled by dropping `cancel_tx` above.
        tokio::spawn(Self::watch_for_file_deletion(
            stream_id,
            stream_name,
            PathBuf::from(file_path),
            cancel_rx,
        ));

        info!("Started recording for stream {stream_id}");
        Ok(file_sink)
    }

    /// Stop recording for a stream. Returns the sink ID for removal from
    /// the pipeline. Clears rotation history and any pending stop reason
    /// since this is the user-facing stop path.
    #[instrument(level = "debug")]
    pub async fn stop_recording(stream_id: &uuid::Uuid) -> Result<uuid::Uuid> {
        Self::stop_recording_inner(stream_id, true).await
    }

    /// Variant used internally by [`crate::stream::manager::Manager::rotate_recording`]
    /// when tearing down the old sink before installing its replacement.
    /// Preserves `rotation_history` (so the cap cumulates across
    /// consecutive rotations) and any stop reason (which is orthogonal
    /// to a rotation in progress).
    #[instrument(level = "debug")]
    pub async fn stop_recording_for_rotation(stream_id: &uuid::Uuid) -> Result<uuid::Uuid> {
        Self::stop_recording_inner(stream_id, false).await
    }

    async fn stop_recording_inner(
        stream_id: &uuid::Uuid,
        reset_cap_state: bool,
    ) -> Result<uuid::Uuid> {
        let mut manager = MANAGER.write().await;

        let mut recording = manager
            .active_recordings
            .remove(stream_id)
            .context(format!("Stream {stream_id} is not recording"))?;

        // Dropping the sender wakes the watcher's `select!` with a
        // `RecvError`, which it treats as cancellation.
        let _ = recording.cancel_tx.take();

        if reset_cap_state {
            manager.rotation_history.remove(stream_id);
        }

        let duration_ms = recording.start_instant.elapsed().as_millis() as u64;
        info!("Stopped recording for stream {stream_id}, duration: {duration_ms}ms");

        Ok(recording.sink_id)
    }

    /// Remove tracking for a stream (e.g. when the stream is removed).
    pub async fn on_stream_removed(stream_id: &uuid::Uuid) {
        let mut manager = MANAGER.write().await;
        if let Some(mut recording) = manager.active_recordings.remove(stream_id) {
            let _ = recording.cancel_tx.take();
            info!("Cleaned up recording state for removed stream {stream_id}");
        }
        manager.rotation_history.remove(stream_id);
        manager.stop_reasons.remove(stream_id);
    }

    /// Record a rotation attempt against the sliding-window cap.
    /// Returns `Ok(())` if the new rotation fits under the cap (and is
    /// therefore appended to history), or `Err(reason)` if honoring it
    /// would exceed `ROTATION_CAP_MAX` within `ROTATION_CAP_WINDOW`.
    pub async fn record_rotation(stream_id: &uuid::Uuid) -> Result<(), String> {
        let mut manager = MANAGER.write().await;
        let history = manager.rotation_history.entry(*stream_id).or_default();

        let now = Instant::now();
        while history
            .front()
            .is_some_and(|t| now.duration_since(*t) > ROTATION_CAP_WINDOW)
        {
            history.pop_front();
        }

        if history.len() >= ROTATION_CAP_MAX {
            return Err(format!(
                "rotation cap exceeded: {} deletions within {} s",
                history.len() + 1,
                ROTATION_CAP_WINDOW.as_secs(),
            ));
        }

        history.push_back(now);
        Ok(())
    }

    /// Attach a stop reason to a stream, to be consumed by the next
    /// `take_stop_reason`. Used by the rotation-cap path so the MAVLink
    /// side can surface it as a `REC ERROR` STATUSTEXT.
    pub async fn set_stop_reason(stream_id: &uuid::Uuid, reason: String) {
        MANAGER
            .write()
            .await
            .stop_reasons
            .insert(*stream_id, reason);
    }

    /// Consume any pending stop reason for a stream. Returns `None` if
    /// the stream was stopped cleanly by the user.
    pub async fn take_stop_reason(stream_id: &uuid::Uuid) -> Option<String> {
        MANAGER.write().await.stop_reasons.remove(stream_id)
    }

    /// Polls `file_path` on `WATCH_POLL_INTERVAL` and triggers rotation
    /// (via [`crate::stream::manager::Manager::rotate_recording`]) when
    /// the file is deleted (`ENOENT`) or replaced (`(dev, ino)` changes
    /// versus the first successful stat). Exits on cancellation.
    ///
    /// The baseline `(dev, ino)` is captured on the first successful
    /// stat rather than at spawn time: `filesink` only opens the file
    /// once the pipeline has transitioned to PLAYING, which happens
    /// after `add_sink` links us into the tee. `WATCH_INITIAL_WAIT`
    /// bounds how long we're willing to wait for that.
    #[instrument(level = "debug", skip(cancel_rx))]
    async fn watch_for_file_deletion(
        stream_id: uuid::Uuid,
        stream_name: String,
        file_path: PathBuf,
        mut cancel_rx: oneshot::Receiver<()>,
    ) {
        let mut baseline: Option<(u64, u64)> = None;
        let started_at = Instant::now();

        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    debug!(
                        "Recording watcher cancelled for stream {stream_id} ({})",
                        file_path.display(),
                    );
                    return;
                }
                _ = tokio::time::sleep(WATCH_POLL_INTERVAL) => {}
            }

            match std::fs::metadata(&file_path) {
                Ok(meta) => {
                    let current = (meta.dev(), meta.ino());
                    match baseline {
                        None => {
                            trace!(
                                "Recording watcher baseline (dev={}, ino={}) for {}",
                                current.0,
                                current.1,
                                file_path.display(),
                            );
                            baseline = Some(current);
                        }
                        Some(base) if base != current => {
                            info!(
                                "Recording file {} replaced (dev/ino {:?} -> {:?}); rotating",
                                file_path.display(),
                                base,
                                current,
                            );
                            Self::spawn_rotation_task(stream_id, stream_name);
                            return;
                        }
                        _ => {}
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if baseline.is_none() {
                        if started_at.elapsed() >= WATCH_INITIAL_WAIT {
                            warn!(
                                "Recording file {} never appeared within {:?}; watcher exiting",
                                file_path.display(),
                                WATCH_INITIAL_WAIT,
                            );
                            return;
                        }
                        continue;
                    }
                    info!("Recording file {} deleted; rotating", file_path.display(),);
                    Self::spawn_rotation_task(stream_id, stream_name);
                    return;
                }
                Err(error) => {
                    warn!(
                        "Recording watcher stat failed on {}: {error}; retrying",
                        file_path.display(),
                    );
                }
            }
        }
    }

    /// Fire-and-forget entry point invoked by the watcher on detection.
    ///
    /// Intentionally **not** `async`: the rotation body transitively
    /// calls back into `try_start` (via `rotate_recording` →
    /// `start_recording` → the retry loop), and inlining that await
    /// chain into the watcher's future creates a self-referential
    /// `async fn` type the compiler cannot prove `Send` for
    /// `tokio::spawn`. Dispatching via a fresh `tokio::spawn` here
    /// breaks the cycle: the new task's `Send` bound is resolved
    /// locally, and the watcher's own future no longer references the
    /// rotation body.
    fn spawn_rotation_task(stream_id: uuid::Uuid, stream_name: String) {
        tokio::spawn(async move {
            if let Err(reason) = Self::record_rotation(&stream_id).await {
                warn!("Recording rotation aborted for {stream_name}: {reason}");
                Self::set_stop_reason(&stream_id, reason).await;
                if let Err(error) =
                    crate::stream::manager::Manager::stop_recording(&stream_id).await
                {
                    warn!("Failed to stop recording after cap exceeded: {error:#}");
                }
                return;
            }

            if let Err(error) =
                crate::stream::manager::Manager::rotate_recording(&stream_id, &stream_name).await
            {
                warn!("Recording rotation failed for {stream_name}: {error:#}");
            }
        });
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            path: "./recordings".to_string(),
            min_free_disk_percent: 10,
        }
    }
}

fn check_disk_space(config: &RecordingConfig) -> Result<()> {
    if config.min_free_disk_percent == 0 {
        return Ok(());
    }

    let (available_bytes, total_bytes) = get_disk_space(&config.path);
    if total_bytes == 0 {
        return Ok(());
    }

    let available_percent = (available_bytes as f64 / total_bytes as f64 * 100.0) as u8;
    if available_percent < config.min_free_disk_percent {
        return Err(anyhow!(
            "Disk space too low: {available_percent}% free, minimum is {}%",
            config.min_free_disk_percent,
        ));
    }

    Ok(())
}

fn get_disk_space(path: &str) -> (u64, u64) {
    let mut system = System::new_all();
    system.refresh_disks();

    let canonical_path =
        std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from("/"));

    let disk = system
        .disks()
        .iter()
        .filter(|d| canonical_path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len());

    match disk {
        Some(disk) => (disk.available_space(), disk.total_space()),
        None => system
            .disks()
            .iter()
            .find(|d| d.mount_point().as_os_str() == "/")
            .map(|d| (d.available_space(), d.total_space()))
            .unwrap_or((0, 0)),
    }
}
