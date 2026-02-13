//! Generalized per-pipeline instrumentation.
//!
//! Replaces the ONVIF-specific `frame_analysis.rs` with a topology-aware
//! system that automatically discovers and probes every element in any
//! GStreamer pipeline (source or sink).
//!
//! ## Stats Levels
//!
//! - **Lite** (`--pipeline-analysis-level lite`): atomic accumulators per pad.
//!   O(1) memory. Provides mean, variance, min, max, throughput.
//! - **Full** (`--pipeline-analysis-level full`): atomic ring buffer per pad.
//!   O(window) memory. Adds percentiles, distributions, stutter detection.
//!
//! ## Probe Installation
//!
//! `install_probes(&pipeline)` iterates all elements, skips bin internals,
//! and installs a GStreamer `BUFFER` probe on **every** pad of each element.
//! Each probe calls `clock_gettime` once (~716ns on Pi4 ARMv7) and records
//! into its own lock-free `PadBuffer`. Sink-pad probes additionally store
//! the wall time into the element's `last_sink_arrival_ns`; src-pad probes
//! compute the per-buffer causal processing time as
//! `wall_ns − last_sink_arrival_ns` and accumulate it in the `ElementProbe`.
//!
//! Multi-pad elements (e.g. `adder`, `compositor`, future audio mixers) get
//! a separate `PadBuffer` per pad, so statistics are never mixed.
//!
//! Probing is fully reactive:
//! - **Static pads** are probed immediately during `install_probes`.
//! - **Dynamic pads** (e.g. rtspsrc "sometimes" src pads, compositor request
//!   sink pads) are automatically probed via `pad-added` signal handlers.
//! - **Dynamically added elements** (e.g. WebRTC sink queue + webrtcbin) are
//!   automatically probed via the pipeline's `element-added` signal.
//! - **Tee src pads** are skipped (they carry duplicate data).

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use arc_swap::ArcSwap;

use rustc_hash::{FxHashMap, FxHashSet};

use gst::prelude::*;
use tracing::*;

use mcm_api::v1::{
    stats::{
        CausalConfidence, Distribution, ElementConnection, ElementSnapshot,
        ElementStats, HealthStatus, IssueKind, PadConnection, PadDirection, PadSnapshot, PadStats,
        PipelineConnection, PipelineSnapshot, PipelineStats, PipelineSummary, RawRecord,
        RestartSnapshot, StatsLevel, StreamSnapshot, StreamsSnapshot, SystemDistribution,
        SystemDistributionAccumulator, SystemSnapshot, ThreadConnection, ThreadSummary, ThreadStats,
    },
    stream::StreamStatus,
};

use super::thread_cpu::ThreadCpuTracker;

use super::element_probe::{ElementProbe, InternalElementSnapshot, InternalPadSnapshot, PadBuffer};

mod diagnostics;
mod health;
mod root_cause;
mod samples;

// ── Internal topology types (no longer in public API) ──

/// A node (element or bin) in the pipeline graph. Internal use only.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct TopologyNode {
    pub name: String,
    pub type_name: String,
    /// Immediate parent bin name (`None` if direct child of the pipeline).
    pub parent_bin: Option<String>,
    /// `true` if this node is a GStreamer Bin (can contain child elements).
    pub is_bin: bool,
}

/// A directed edge between two pads. Internal use only.
#[derive(Debug, Clone)]
pub(crate) struct TopologyEdge {
    pub from_node: String,
    pub from_pad: String,
    pub to_node: String,
    pub to_pad: String,
    pub media_type: Option<String>,
}

/// Sink sub-pipeline metadata. Internal use only.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SinkInfo {
    pub id: String,
    pub sink_type: String,
    pub tee: String,
    pub elements: Vec<String>,
}

/// Pipeline topology graph. Internal use only.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct PipelineTopology {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
    pub sinks: Vec<SinkInfo>,
}

/// Internal edge delay (computation intermediate, not in public API).
/// Stores `edge_idx` into the `PipelineTopology.edges` array to avoid
/// cloning element name strings on every snapshot.
#[derive(Clone)]
struct InternalEdgeDelay {
    edge_idx: usize,
    freshness_delay_ms: f64,
    causal_latency_ms: Option<mcm_api::v1::stats::Distribution>,
    causal_match_rate: Option<f64>,
    causal_matched_samples: Option<u64>,
    causal_confidence: Option<CausalConfidence>,
}

/// Internal thread group (computation intermediate, not in public API).
#[derive(Clone)]
struct InternalThreadGroup {
    thread_id: u32,
    thread_name: Option<String>,
    cpu_pct: f64,
    cpu_stats: Option<SystemDistribution>,
    elements: Vec<String>,
}

// ── Configuration ──

/// Default rolling window size (~30s at 30fps).
pub const DEFAULT_WINDOW_SIZE: usize = 900;
/// Maximum allowed rolling window size.
pub const MAX_WINDOW_SIZE: usize = 50_000;
/// Capacity for per-pipeline and per-thread CPU history ring buffers (~2min at 1/sec).
const CPU_HISTORY_CAPACITY: usize = 120;

/// Global stats level (switchable at runtime via API).
static STATS_LEVEL: AtomicU8 = AtomicU8::new(0); // 0 = Lite, 1 = Full

/// Global window size (applies to newly created pipelines).
static WINDOW_SIZE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(DEFAULT_WINDOW_SIZE);

pub fn global_stats_level() -> StatsLevel {
    match STATS_LEVEL.load(Ordering::Relaxed) {
        1 => StatsLevel::Full,
        _ => StatsLevel::Lite,
    }
}

pub fn set_global_stats_level(level: StatsLevel) {
    STATS_LEVEL.store(
        match level {
            StatsLevel::Lite => 0,
            StatsLevel::Full => 1,
        },
        Ordering::Relaxed,
    );
}

pub fn global_window_size() -> usize {
    WINDOW_SIZE.load(Ordering::Relaxed)
}

pub fn set_global_window_size(size: usize) {
    WINDOW_SIZE.store(clamp_window_size(size), Ordering::Relaxed);
}

pub fn is_valid_window_size(size: usize) -> bool {
    (1..=MAX_WINDOW_SIZE).contains(&size)
}

fn clamp_window_size(size: usize) -> usize {
    size.clamp(1, MAX_WINDOW_SIZE)
}

// ── Global Registry ──

static REGISTRY: std::sync::LazyLock<Mutex<Vec<Arc<PipelineAnalysis>>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

static RESTART_TRACKER: std::sync::LazyLock<Mutex<HashMap<String, RestartTracker>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static SYSTEM_SAMPLER_STARTED: AtomicBool = AtomicBool::new(false);

// ── Snapshot cache ──
//
// Multiple WebSocket handlers tick every ~1s and all call `snapshot_all()`.
// Without caching, N+1 concurrent callers each independently compute full
// distributions (6 sorts of ~900 elements per pad), edge delays, causal
// latency, CPU attribution, etc. — all producing identical results.
//
// The cache stores the last computed result behind an `Arc` with a short TTL.
// Concurrent readers within the same tick share one computation.

/// How long a cached snapshot is considered fresh.
const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(900);

/// Cache of readable property names per element type name.
/// Populated once per element type; property lists are class-level and immutable.
static PROP_NAME_CACHE: std::sync::LazyLock<Mutex<HashMap<String, Arc<Vec<String>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// GObject property names excluded from `element-deep-info` snapshots.
///
/// These are either extremely large (raw buffer data), contain opaque GObject
/// pointers that are meaningless in JSON, or are internal bookkeeping fields.
const PROPERTY_DENY_LIST: &[&str] = &[
    // Identity / bookkeeping — already represented in the snapshot structure.
    "name",
    "parent",
    // Raw buffer/frame data — `last-sample` can be 16+ MB for a single video frame.
    "last-sample",
    // GObject pointer references — opaque addresses, useless in JSON.
    "proxysink",
    "socket",
    "socket-v6",
    "used-socket",
    "used-socket-v6",
    "alloc-pad",
    // Debug messages — almost always NULL, adds noise.
    "last-message",
];

/// Cached intermediate snapshots: `(stream_id, pipeline_name, InternalPipelineData)`.
/// The final conversion to API types happens in `full_snapshot()`.
#[derive(Clone)]
struct InternalPipelineData {
    name: String,
    stream_id: String,
    stats_level: StatsLevel,
    window_size: usize,
    expected_interval_ms: f64,
    uptime_secs: f64,
    elements: FxHashMap<String, InternalElementSnapshot>,
    edges: Vec<InternalEdgeDelay>,
    summary: PipelineSummary,
    topology: Option<Arc<PipelineTopology>>,
    system: SystemSnapshot,
    restarts: RestartSnapshot,
    pipeline_cpu_pct: Option<f64>,
    pipeline_cpu_stats: Option<SystemDistribution>,
    thread_groups: Vec<InternalThreadGroup>,
    element_cpu_history: FxHashMap<String, Vec<f64>>,
}

/// Lock-free snapshot cache entry: (timestamp, data).
struct SnapshotCacheEntry {
    ts: Instant,
    data: Arc<Vec<InternalPipelineData>>,
}

static SNAPSHOT_CACHE: std::sync::LazyLock<ArcSwap<Option<SnapshotCacheEntry>>> =
    std::sync::LazyLock::new(|| ArcSwap::from_pointee(None));

/// Lock-free JSON snapshot cache entry: (timestamp, buffer_limit, json_bytes).
struct JsonCacheEntry {
    ts: Instant,
    buffer_limit: usize,
    json: Arc<Vec<u8>>,
}

static JSON_SNAPSHOT_CACHE: std::sync::LazyLock<ArcSwap<Option<JsonCacheEntry>>> =
    std::sync::LazyLock::new(|| ArcSwap::from_pointee(None));

/// Register a new analysis instance (called when a pipeline starts).
pub fn register(analysis: &Arc<PipelineAnalysis>) {
    let name = &analysis.pipeline_name;

    // Track restarts
    if let Ok(mut tracker) = RESTART_TRACKER.lock() {
        if let Some(entry) = tracker.get_mut(name) {
            entry.record_restart();
        } else {
            tracker.insert(name.clone(), RestartTracker::new());
        }
    }

    if let Ok(mut reg) = REGISTRY.lock() {
        reg.retain(|a| Arc::strong_count(a) > 1 && a.pipeline_name != analysis.pipeline_name);
        reg.push(analysis.clone());
    }
}

/// Snapshot all active pipeline analyses as internal data.
///
/// Returns an `Arc`-wrapped result that is cached for up to [`SNAPSHOT_CACHE_TTL`].
/// Multiple concurrent callers (e.g. several WebSocket handlers ticking at 1Hz)
/// will share a single computation instead of each independently computing
/// distributions, edge delays, and CPU attribution.
fn snapshot_all_internal() -> Arc<Vec<InternalPipelineData>> {
    // Fast path: lock-free cache check via ArcSwap
    let guard = SNAPSHOT_CACHE.load();
    if let Some(ref entry) = **guard {
        if entry.ts.elapsed() < SNAPSHOT_CACHE_TTL {
            return Arc::clone(&entry.data);
        }
    }
    drop(guard);

    // Cache miss — compute fresh snapshot
    let analyses = active_analyses();
    let snapshots = if analyses.is_empty() {
        Vec::new()
    } else {
        let restarts = restart_snapshots();
        thread_local! {
            static SCRATCH: std::cell::RefCell<MergeJoinScratch> =
                std::cell::RefCell::new(MergeJoinScratch::new());
        }
        SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            analyses
                .iter()
                .map(|a| {
                    let restart = restarts.get(&a.pipeline_name).cloned().unwrap_or_default();
                    a.snapshot_internal(restart, &mut scratch)
                })
                .collect()
        })
    };

    let result = Arc::new(snapshots);
    SNAPSHOT_CACHE.store(Arc::new(Some(SnapshotCacheEntry {
        ts: Instant::now(),
        data: Arc::clone(&result),
    })));
    result
}

/// Produce a consolidated snapshot of all streams in the new hierarchical format.
///
/// Uses `streams_info` (from the stream manager) as the authoritative list of
/// configured streams, enriching each with pipeline analysis data when available.
/// Streams without analysis data appear with an empty `pipelines` vec.
///
/// This is the backing function for `GET /stats/streams/snapshot`.
pub fn full_snapshot(buffer_limit: usize, streams_info: &[StreamStatus]) -> StreamsSnapshot {
    use std::time::{SystemTime, UNIX_EPOCH};

    let internal_arc = snapshot_all_internal();
    let internal = &*internal_arc;

    // Group pipeline analysis data by stream_id (borrow only, no deep clone).
    let mut pipeline_map: FxHashMap<&str, Vec<&InternalPipelineData>> = FxHashMap::default();
    for ipd in internal {
        pipeline_map.entry(&ipd.stream_id).or_default().push(ipd);
    }

    // Build streams from the authoritative stream manager list.
    let mut streams: Vec<StreamSnapshot> = streams_info
        .iter()
        .map(|info| {
            let stream_id = info.id.to_string();
            let pipelines: Vec<PipelineSnapshot> = pipeline_map
                .remove(stream_id.as_str())
                .unwrap_or_default()
                .into_iter()
                .map(|ipd| build_api_pipeline_snapshot(ipd, buffer_limit))
                .collect();
            let stats = health::compute_stream_stats(&pipelines, info.running);
            StreamSnapshot {
                id: stream_id,
                name: info.video_and_stream.name.clone(),
                running: info.running,
                error: info.error.clone(),
                video_and_stream: info.video_and_stream.clone(),
                mavlink: info.mavlink.clone(),
                stats,
                pipelines,
            }
        })
        .collect();

    // Populate inter-pipeline connections by matching proxy/shm bridge elements.
    // For each proxysink in pipeline A, find the proxysrc with the same numeric
    // suffix in pipeline B → create a PipelineConnection on A pointing to B.
    for stream in &mut streams {
        derive_pipeline_connections(&mut stream.pipelines);
    }

    let fleet_stats = health::compute_fleet_stats(&streams);

    let timestamp_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    StreamsSnapshot {
        timestamp_ns,
        stats: fleet_stats,
        streams,
    }
}

/// Return a serialized-JSON snapshot, using a cache with the same TTL as the
/// internal snapshot cache. On cache hit (same TTL window + same buffer_limit),
/// returns the pre-serialized bytes directly — no snapshot computation, no
/// `serde_json` work. On miss, delegates to [`full_snapshot`] and serializes.
pub fn full_snapshot_json(buffer_limit: usize, streams_info: &[StreamStatus]) -> Arc<Vec<u8>> {
    // Lock-free cache check via ArcSwap
    let guard = JSON_SNAPSHOT_CACHE.load();
    if let Some(ref entry) = **guard {
        if entry.ts.elapsed() < SNAPSHOT_CACHE_TTL && entry.buffer_limit == buffer_limit {
            return Arc::clone(&entry.json);
        }
    }
    drop(guard);

    let snapshot = full_snapshot(buffer_limit, streams_info);
    let json = serde_json::to_vec(&snapshot).unwrap_or_default();
    let result = Arc::new(json);
    JSON_SNAPSHOT_CACHE.store(Arc::new(Some(JsonCacheEntry {
        ts: Instant::now(),
        buffer_limit,
        json: Arc::clone(&result),
    })));
    result
}

/// Invalidate the snapshot cache. Called after reset/state changes.
pub fn invalidate_snapshot_cache() {
    SNAPSHOT_CACHE.store(Arc::new(None));
    JSON_SNAPSHOT_CACHE.store(Arc::new(None));
}

/// Update the pipeline topology for a pipeline (matched by GStreamer pipeline name).
pub(crate) fn update_pipeline_topology(pipeline_name: &str, topology: PipelineTopology) {
    for a in active_analyses() {
        if a.pipeline_name == pipeline_name {
            a.set_topology(topology);
            return;
        }
    }
}

/// Reset all statistics and re-enable recording.
pub fn reset_all() {
    for a in active_analyses() {
        a.reset();
        a.enabled.store(true, Ordering::Relaxed);
    }
    invalidate_snapshot_cache();
}

/// Fast-path: read raw records from a specific pipeline's element probe ring
/// buffer without computing full distributions, edge delays, or CPU attribution.
///
/// This is orders of magnitude cheaper than [`snapshot_all()`] for the samples
/// API, which only needs the raw record timeseries from one pad.
///
/// Returns `(element_name, pad_name, records)` for the best-matching pad.
pub fn read_pipeline_raw_records(
    pipeline_name: &str,
    element_filter: Option<&str>,
    pad_filter: Option<&str>,
) -> Option<(String, String, Vec<RawRecord>)> {
    let analyses = active_analyses();
    let analysis = analyses.iter().find(|a| a.pipeline_name == pipeline_name)?;
    let elements = analysis.elements.lock().ok()?;

    let mut best: Option<(String, String, Vec<RawRecord>, u64)> = None;

    let iter: Box<dyn Iterator<Item = (&String, &Arc<super::element_probe::ElementProbe>)>> =
        if let Some(name) = element_filter {
            Box::new(elements.iter().filter(move |(n, _)| n.as_str() == name))
        } else {
            Box::new(elements.iter())
        };

    for (el_name, probe) in iter {
        if let Some((pad_name, records, total)) = probe.read_raw_records_from_best_pad(pad_filter) {
            if best.as_ref().map(|(_, _, _, s)| total > *s).unwrap_or(true) {
                best = Some((el_name.clone(), pad_name.to_string(), records, total));
            }
        }
    }

    best.map(|(e, p, r, _)| (e, p, r))
}

/// Start the global (host-level) sampler task once.
pub fn ensure_global_system_sampler_started() {
    if SYSTEM_SAMPLER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut prev_cpu: Option<(u64, u64)> = None;
        loop {
            interval.tick().await;
            sample_all_analyses(&mut prev_cpu);
        }
    });
}

fn sample_all_analyses(prev_cpu: &mut Option<(u64, u64)>) {
    let metrics = crate::stream::stats::system_metrics::snapshot(prev_cpu);
    for a in active_analyses() {
        if !a.is_enabled() {
            continue;
        }
        if let Ok(mut sys) = a.system.lock() {
            sys.record(
                metrics.cpu_pct,
                metrics.load_1m,
                metrics.mem_used_pct,
                metrics.temperature_c,
            );
        }
        a.poll_thread_cpu();
    }
}

fn active_analyses() -> Vec<Arc<PipelineAnalysis>> {
    if let Ok(reg) = REGISTRY.lock() {
        let mut out = Vec::with_capacity(reg.len());
        for a in reg.iter() {
            if Arc::strong_count(a) > 1 {
                out.push(Arc::clone(a));
            }
        }
        return out;
    }
    Vec::new()
}

fn restart_snapshots() -> FxHashMap<String, RestartSnapshot> {
    if let Ok(tracker) = RESTART_TRACKER.lock() {
        return tracker
            .iter()
            .map(|(name, restart)| (name.clone(), restart.snapshot()))
            .collect();
    }
    FxHashMap::default()
}

// ── Per-pipeline analysis ──

/// Per-pipeline analysis container. Holds per-element probes and metadata.
pub struct PipelineAnalysis {
    pub pipeline_name: String,
    pub stream_id: String,
    pub expected_interval_ms: f64,
    pub enabled: AtomicBool,

    /// Per-element probes, keyed by element name. Populated by `install_probes`.
    elements: Mutex<HashMap<String, Arc<ElementProbe>>>,

    /// Pipeline topology (adjacency graph).
    topology: Mutex<Option<Arc<PipelineTopology>>>,

    /// System metrics (1 sample/sec).
    pub system: Mutex<SystemBuffer>,

    /// Per-thread CPU tracker. Polled 1/sec alongside system metrics.
    thread_cpu: Mutex<ThreadCpuTracker>,

    /// Ring buffer of pipeline-total CPU samples (1/sec), for windowed stats.
    pipeline_cpu_history: Mutex<VecDeque<f64>>,

    /// Per-element CPU history ring buffers (1/sec), keyed by element name.
    /// Populated during `poll_thread_cpu()` via lightweight attribution.
    element_cpu_history: Mutex<HashMap<String, VecDeque<f64>>>,

    /// Stats level at creation time (determines buffer type).
    stats_level: StatsLevel,
    window_size: usize,

    /// When this analysis was created.
    created_at: Instant,

    /// Weak reference to the GStreamer pipeline for introspecting element
    /// state, properties, pad caps, and queue fill levels at snapshot time.
    gst_pipeline: Mutex<Option<gst::glib::WeakRef<gst::Pipeline>>>,
}

impl PipelineAnalysis {
    pub fn new(pipeline_name: String, stream_id: String, expected_interval_ms: f64) -> Self {
        let level = global_stats_level();
        let window = global_window_size();
        Self {
            pipeline_name,
            stream_id,
            expected_interval_ms,
            enabled: AtomicBool::new(true),
            elements: Mutex::new(HashMap::new()),
            topology: Mutex::new(None),
            system: Mutex::new(SystemBuffer::new(CPU_HISTORY_CAPACITY)),
            thread_cpu: Mutex::new(ThreadCpuTracker::new()),
            pipeline_cpu_history: Mutex::new(VecDeque::with_capacity(CPU_HISTORY_CAPACITY)),
            element_cpu_history: Mutex::new(HashMap::new()),
            stats_level: level,
            window_size: window,
            created_at: Instant::now(),
            gst_pipeline: Mutex::new(None),
        }
    }

    /// Is recording currently enabled?
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Direct access to the elements map for the benchmark harness.
    #[cfg(feature = "bench-internal")]
    pub fn elements_lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<ElementProbe>>> {
        self.elements.lock().unwrap()
    }

    /// Direct access to the topology slot for the benchmark harness.
    #[cfg(feature = "bench-internal")]
    pub(crate) fn topology_lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<PipelineTopology>>> {
        self.topology.lock().unwrap()
    }

    /// Install probes on all elements in the given GStreamer pipeline.
    ///
    /// For each element, creates an `ElementProbe` with the appropriate stats
    /// level and installs `BUFFER | BUFFER_LIST` probes on existing pads.
    /// Connects `pad-added` on each element for dynamic pads (e.g. rtspsrc)
    /// and `deep-element-added` on the pipeline for dynamically added elements
    /// at any depth (e.g. rtspsrc internals, WebRTC sink queue + webrtcbin).
    pub fn install_probes(self: &Arc<Self>, pipeline: &gst::Pipeline) {
        for el in pipeline.iterate_recurse().into_iter().filter_map(Result::ok) {
            self.install_element_probe(&el);
        }

        // Watch for dynamically added elements at any depth in the pipeline
        // hierarchy (e.g. rtspsrc internals, WebRTC sink queue + webrtcbin).
        {
            let analysis = Arc::clone(self);
            pipeline.connect_deep_element_added(move |_pipeline, _sub_bin, element| {
                if analysis.install_element_probe(element) {
                    debug!(
                        "Auto-probed new element {:?} via deep-element-added",
                        element.name()
                    );
                }
            });
        }

        // Clean up element probes when elements are removed at any depth
        // (e.g. WebRTC sink teardown, rtspsrc internal cleanup). Without this,
        // dead elements and their stale thread CPU data persist in snapshots.
        {
            let analysis = Arc::clone(self);
            pipeline.connect_deep_element_removed(move |_pipeline, _sub_bin, element| {
                let el_name = element.name().to_string();
                if let Ok(mut elements) = analysis.elements.lock() {
                    if elements.remove(&el_name).is_some() {
                        debug!("Removed probe for element {el_name:?} via deep-element-removed");
                    }
                }
            });
        }

        // Extract and store the pipeline topology.
        let topo = crate::stream::pipeline::runner::extract_topology(pipeline);
        if let Ok(mut t) = self.topology.lock() {
            *t = Some(Arc::new(topo));
        }

        // Store a weak reference to the pipeline for snapshot-time introspection.
        {
            let weak = pipeline.downgrade();
            if let Ok(mut slot) = self.gst_pipeline.lock() {
                *slot = Some(weak);
            }
        }
    }

    /// Install probes on a single GStreamer element.
    ///
    /// Creates an `ElementProbe` and registers a `PadBuffer` for every existing
    /// pad (sink **and** src). Also connects a `pad-added` handler so that
    /// dynamic pads (e.g. rtspsrc "sometimes" src pads, compositor request sink
    /// pads) are automatically probed when they appear.
    ///
    /// For tee elements, src pads are skipped (they carry duplicate data).
    ///
    /// Returns `true` if probes were installed, `false` if the element was
    /// skipped (already tracked, etc.).
    fn install_element_probe(self: &Arc<Self>, el: &gst::Element) -> bool {
        let el_name = el.name().to_string();
        let factory_name = el
            .factory()
            .map(|f| f.name().to_string())
            .unwrap_or_default();

        // Skip if already tracked (race guard for element-added handler).
        if self
            .elements
            .lock()
            .ok()
            .map(|e| e.contains_key(&el_name))
            .unwrap_or(false)
        {
            return false;
        }

        let is_tee = factory_name == "tee" || el_name.contains("Tee");

        let parent_bin: Option<Arc<str>> = el
            .parent()
            .and_then(|p| p.downcast::<gst::Element>().ok())
            .filter(|p| !p.is::<gst::Pipeline>())
            .map(|p| Arc::from(p.name().as_str()));

        let probe = Arc::new(ElementProbe::new(
            el_name.clone(),
            factory_name.clone(),
            self.stats_level,
            self.window_size,
            is_tee, // skip_src_pads for tee elements
            parent_bin,
        ));

        // Install probes on ALL existing sink pads.
        for pad in el.sink_pads() {
            if let Some(pad_buf) = probe.register_sink_pad(pad.name().as_ref()) {
                install_pad_probe(&pad, &pad_buf, self, &probe, gst::PadDirection::Sink);
            }
        }

        // Install probes on ALL existing src pads (skipped for tee by register_src_pad).
        for pad in el.src_pads() {
            if let Some(pad_buf) = probe.register_src_pad(pad.name().as_ref()) {
                install_pad_probe(&pad, &pad_buf, self, &probe, gst::PadDirection::Src);
            }
        }

        // Watch for future dynamic pads (e.g. rtspsrc "sometimes" src pads,
        // compositor request sink pads). Each pad gets its own PadBuffer;
        // register_*_pad() returns None for duplicates or skipped directions.
        {
            let ep = Arc::clone(&probe);
            let analysis = Arc::clone(self);
            el.connect_pad_added(move |el, pad| {
                let pad_name = pad.name().to_string();
                let pad_buf = match pad.direction() {
                    gst::PadDirection::Sink => ep.register_sink_pad(&pad_name),
                    gst::PadDirection::Src => ep.register_src_pad(&pad_name),
                    _ => None,
                };
                if let Some(pad_buf) = pad_buf {
                    install_pad_probe(pad, &pad_buf, &analysis, &ep, pad.direction());
                    debug!(
                        "Auto-probed dynamic {:?} pad {pad_name:?} on {:?}",
                        pad.direction(),
                        el.name()
                    );
                }
            });
        }

        if let Ok(mut elems) = self.elements.lock() {
            elems.insert(el_name.clone(), probe);
        }

        debug!(
            "Probes installed on {el_name:?} (type={factory_name:?}) for pipeline {:?}",
            self.pipeline_name
        );

        true
    }

    /// Set the pipeline topology.
    pub(crate) fn set_topology(&self, topo: PipelineTopology) {
        if let Ok(mut t) = self.topology.lock() {
            *t = Some(Arc::new(topo));
        }
    }

    /// Reset all per-pipeline statistics while keeping probes installed.
    pub fn reset(&self) {
        if let Ok(elements) = self.elements.lock() {
            for probe in elements.values() {
                probe.reset();
            }
        }
        if let Ok(mut system) = self.system.lock() {
            system.reset();
        }
        if let Ok(mut tracker) = self.thread_cpu.lock() {
            tracker.reset();
        }
        if let Ok(mut hist) = self.pipeline_cpu_history.lock() {
            hist.clear();
        }
        if let Ok(mut hist) = self.element_cpu_history.lock() {
            hist.clear();
        }
    }

    /// Poll per-thread CPU usage for all streaming threads in this pipeline.
    ///
    /// Collects the unique thread IDs from all probed elements, then reads
    /// `/proc/self/task/{tid}/stat` for each to compute delta CPU usage.
    /// After polling threads, runs lightweight per-element CPU attribution
    /// and pushes the values into per-element history ring buffers.
    /// Called once per second from the system-metrics polling task.
    pub fn poll_thread_cpu(&self) {
        // Collect element attribution hints and unique TIDs in a single lock.
        let element_hints: Vec<(String, Option<u32>, Option<f64>)> =
            if let Ok(elements) = self.elements.lock() {
                let mut hints = Vec::with_capacity(elements.len());
                for (name, probe) in elements.iter() {
                    let (tid, pt) = probe.cpu_attribution_hint();
                    hints.push((name.clone(), tid, pt));
                }
                hints
            } else {
                return;
            };

        let mut tids_set =
            FxHashSet::with_capacity_and_hasher(element_hints.len(), Default::default());
        for (_, tid, _) in &element_hints {
            if let Some(t) = tid {
                tids_set.insert(*t);
            }
        }
        let tids: Vec<u32> = tids_set.into_iter().collect();

        if tids.is_empty() {
            return;
        }

        let total_cpu = if let Ok(mut tracker) = self.thread_cpu.lock() {
            tracker.poll(&tids);
            tids.iter()
                .filter_map(|&tid| tracker.get(tid))
                .map(|info| info.cpu_pct)
                .sum::<f64>()
        } else {
            return;
        };

        if let Ok(mut hist) = self.pipeline_cpu_history.lock() {
            if hist.len() >= CPU_HISTORY_CAPACITY {
                hist.pop_front();
            }
            hist.push_back(total_cpu);
        }

        // Lightweight per-element CPU attribution (same logic as attribute_cpu).
        self.attribute_cpu_to_history(&element_hints);
    }

    /// Lightweight CPU attribution for the 1 Hz poll cycle.
    ///
    /// Uses element hints (thread_id, processing_time_us) collected from
    /// atomic reads — no full snapshot needed. Pushes per-element CPU values
    /// into history ring buffers for windowed statistics.
    fn attribute_cpu_to_history(&self, hints: &[(String, Option<u32>, Option<f64>)]) {
        let tracker = match self.thread_cpu.lock() {
            Ok(t) => t,
            Err(_) => return,
        };

        // Group elements by thread ID.
        let mut tid_to_elements: FxHashMap<u32, Vec<(String, Option<f64>)>> = FxHashMap::default();
        for (name, tid, pt) in hints {
            if let Some(tid) = tid {
                tid_to_elements
                    .entry(*tid)
                    .or_default()
                    .push((name.clone(), *pt));
            }
        }

        // Per-element attributed CPU values.
        let mut element_cpu: FxHashMap<String, f64> =
            FxHashMap::with_capacity_and_hasher(hints.len(), Default::default());

        for (tid, elements) in &tid_to_elements {
            let thread_cpu_pct = tracker.get(*tid).map(|i| i.cpu_pct).unwrap_or(0.0);
            if thread_cpu_pct <= 0.0 {
                continue;
            }

            let total_proc_time: f64 = elements.iter().filter_map(|(_, pt)| *pt).sum();

            let mut attributed = 0.0;
            for (name, pt) in elements {
                if let Some(pt) = pt {
                    if total_proc_time > 0.0 {
                        let cpu = (pt / total_proc_time) * thread_cpu_pct;
                        element_cpu.insert(name.clone(), cpu);
                        attributed += cpu;
                    }
                }
            }

            let mut unmeasured: Vec<&String> = Vec::with_capacity(elements.len());
            for (name, pt) in elements {
                if pt.is_none() {
                    unmeasured.push(name);
                }
            }

            if !unmeasured.is_empty() {
                let residual = (thread_cpu_pct - attributed).max(0.0);
                let share = residual / unmeasured.len() as f64;
                for name in unmeasured {
                    element_cpu.insert(name.clone(), share);
                }
            }
        }

        // Push into history ring buffers.
        if let Ok(mut hist_map) = self.element_cpu_history.lock() {
            for (name, cpu) in &element_cpu {
                let hist = hist_map
                    .entry(name.clone())
                    .or_insert_with(|| VecDeque::with_capacity(CPU_HISTORY_CAPACITY));
                if hist.len() >= CPU_HISTORY_CAPACITY {
                    hist.pop_front();
                }
                hist.push_back(*cpu);
            }

            // Prune stale elements no longer in the pipeline.
            let current_names: FxHashSet<&String> = element_cpu.keys().collect();
            hist_map.retain(|name, _| current_names.contains(name));
        }
    }

    /// Produce internal snapshot data for this pipeline's instrumentation.
    fn snapshot_internal(
        &self,
        restarts: RestartSnapshot,
        scratch: &mut MergeJoinScratch,
    ) -> InternalPipelineData {
        // Collect per-element snapshots while holding the elements lock, then
        // release it BEFORE calling enrich_with_gst_introspection. This avoids
        // an AB-BA deadlock: snapshot holds elements→needs GStreamer bin lock,
        // while connect_element_{added,removed} holds bin lock→needs elements.
        let mut element_snapshots: FxHashMap<String, InternalElementSnapshot>;
        {
            let elements_map = self.elements.lock().ok();
            let elem_count = elements_map.as_ref().map(|e| e.len()).unwrap_or(0);
            element_snapshots =
                FxHashMap::with_capacity_and_hasher(elem_count, Default::default());
            if let Some(ref elems) = elements_map {
                for (name, probe) in elems.iter() {
                    element_snapshots.insert(name.clone(), probe.snapshot());
                }
            }
            // elements_map MutexGuard dropped here
        }

        let topo = self
            .topology
            .lock()
            .ok()
            .and_then(|t| t.as_ref().map(Arc::clone));
        let system = self
            .system
            .lock()
            .ok()
            .map(|mut s| s.snapshot())
            .unwrap_or_default();

        // Compute inter-element edge delays from topology adjacency.
        let edges = compute_edge_delays(&element_snapshots, topo.as_deref(), scratch);

        // Compute pipeline summary.
        let summary = compute_summary(
            &element_snapshots,
            &edges,
            topo.as_deref(),
            &mut scratch.pts_scratch,
        );

        // Compute CPU attribution: thread groups, per-element processing time,
        // and per-element estimated CPU usage.
        let (pipeline_cpu_pct, thread_groups) =
            self.compute_cpu_attribution(&mut element_snapshots);

        let pipeline_cpu_stats = self.pipeline_cpu_history.lock().ok().and_then(|hist| {
            if hist.is_empty() {
                None
            } else {
                let mut data = Vec::with_capacity(hist.len());
                data.extend(hist.iter());
                Some(SystemDistribution::from_slice(&data))
            }
        });

        // In Full mode, compute PTS-matched intra-element processing time
        // distributions. This gives accurate per-buffer transit times even for
        // cross-thread elements (queues), and provides percentile statistics.
        if self.stats_level == StatsLevel::Full {
            compute_intra_element_processing_time(&mut element_snapshots, scratch);
        }

        // Enrich element/pad snapshots with GStreamer introspection data.
        // Safe to call: self.elements lock is not held here.
        self.enrich_with_gst_introspection(&mut element_snapshots);

        InternalPipelineData {
            name: self.pipeline_name.clone(),
            stream_id: self.stream_id.clone(),
            stats_level: self.stats_level,
            window_size: self.window_size,
            expected_interval_ms: self.expected_interval_ms,
            uptime_secs: self.created_at.elapsed().as_secs_f64(),
            elements: element_snapshots,
            edges,
            summary,
            topology: topo,
            system,
            restarts,
            pipeline_cpu_pct: if pipeline_cpu_pct > 0.0 {
                Some(pipeline_cpu_pct)
            } else {
                None
            },
            pipeline_cpu_stats,
            thread_groups,
            element_cpu_history: self
                .element_cpu_history
                .lock()
                .ok()
                .map(|hist| {
                    let mut map =
                        FxHashMap::with_capacity_and_hasher(hist.len(), Default::default());
                    for (name, deque) in hist.iter() {
                        let mut v = Vec::with_capacity(deque.len());
                        v.extend(deque.iter());
                        map.insert(name.clone(), v);
                    }
                    map
                })
                .unwrap_or_default(),
        }
    }

    /// Compute CPU attribution for all elements in this pipeline.
    ///
    /// Acquires the thread CPU tracker lock and delegates to [`attribute_cpu`].
    ///
    /// Returns `(total_pipeline_cpu_pct, thread_groups)`.
    fn compute_cpu_attribution(
        &self,
        element_snapshots: &mut FxHashMap<String, InternalElementSnapshot>,
    ) -> (f64, Vec<InternalThreadGroup>) {
        let start = Instant::now();

        let tracker = match self.thread_cpu.lock() {
            Ok(t) => t,
            Err(_) => return (0.0, Vec::new()),
        };

        let result = attribute_cpu(element_snapshots, &tracker);

        trace!(
            pipeline = %self.pipeline_name,
            elapsed_us = start.elapsed().as_micros(),
            thread_count = result.1.len(),
            element_count = element_snapshots.len(),
            "cpu_attribution"
        );

        result
    }

    /// Enrich element and pad snapshots with data from the live GStreamer pipeline.
    ///
    /// Runs on the cold snapshot path (~1Hz). Upgrades the weak pipeline ref,
    /// iterates elements, and fills in:
    /// - `pad-caps`: `InternalPadSnapshot.caps` from `pad.current_caps()`
    /// - `element-deep-info`: element state, GObject properties, queue fill levels
    fn enrich_with_gst_introspection(
        &self,
        element_snapshots: &mut FxHashMap<String, InternalElementSnapshot>,
    ) {
        let pipeline = match self
            .gst_pipeline
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(|weak| weak.upgrade()))
        {
            Some(p) => p,
            None => return,
        };

        for gst_el in pipeline.children() {
            let el_name = gst_el.name().to_string();
            let Some(snap) = element_snapshots.get_mut(&el_name) else {
                continue;
            };

            {
                // Element state
                let (_, current, _) = gst_el.state(gst::ClockTime::ZERO);
                snap.state = Some(
                    match current {
                        gst::State::VoidPending => "void-pending",
                        gst::State::Null => "null",
                        gst::State::Ready => "ready",
                        gst::State::Paused => "paused",
                        gst::State::Playing => "playing",
                        _ => "unknown",
                    }
                    .to_string(),
                );

                // GObject properties: cache the filtered name list per element type
                // (list_properties() returns class-level data that never changes)
                let prop_names = {
                    let type_name = &snap.element_type;
                    let cached = PROP_NAME_CACHE
                        .lock()
                        .ok()
                        .and_then(|cache| cache.get(&**type_name).cloned());
                    if let Some(names) = cached {
                        names
                    } else {
                        let names: Vec<String> = gst_el
                            .list_properties()
                            .iter()
                            .filter(|p| {
                                let name = p.name();
                                p.flags().contains(gst::glib::ParamFlags::READABLE)
                                    && !PROPERTY_DENY_LIST.contains(&name.as_ref())
                            })
                            .map(|p| p.name().to_string())
                            .collect();
                        let names = Arc::new(names);
                        if let Ok(mut cache) = PROP_NAME_CACHE.lock() {
                            cache.insert(type_name.to_string(), Arc::clone(&names));
                        }
                        names
                    }
                };
                let props: Vec<(String, String)> = prop_names
                    .iter()
                    .filter_map(|name| {
                        let val = gst_el.property_value(name);
                        let value_str = val.serialize().map(|s| s.to_string()).ok()?;
                        Some((name.clone(), value_str))
                    })
                    .collect();
                snap.properties = if props.is_empty() { None } else { Some(props) };

                // Queue fill levels
                if snap
                    .element_type
                    .as_bytes()
                    .windows(5)
                    .any(|w| w.eq_ignore_ascii_case(b"queue"))
                {
                    use super::element_probe::InternalQueueStats;

                    let get_u32 = |prop: &str| -> u32 {
                        gst_el.property_value(prop).get::<u32>().unwrap_or(0)
                    };
                    let get_u64 = |prop: &str| -> u64 {
                        gst_el
                            .property_value(prop)
                            .get::<u64>()
                            .or_else(|_| gst_el.property_value(prop).get::<u32>().map(|v| v as u64))
                            .unwrap_or(0)
                    };
                    let get_time = |prop: &str| -> u64 {
                        gst_el.property_value(prop).get::<u64>().unwrap_or(0)
                    };

                    snap.queue_stats = Some(InternalQueueStats {
                        current_level_buffers: get_u32("current-level-buffers"),
                        current_level_bytes: get_u64("current-level-bytes"),
                        current_level_time_ns: get_time("current-level-time"),
                        max_level_buffers: get_u32("max-size-buffers"),
                        max_level_bytes: get_u64("max-size-bytes"),
                        max_level_time_ns: get_time("max-size-time"),
                    });
                }
            }

            // Pad caps
            {
                for pad in gst_el.pads() {
                    let pad_name = pad.name().to_string();
                    let caps_str = pad.current_caps().map(|c| c.to_string());

                    // Find matching internal pad and set caps
                    let pads_list = snap.sink_pads.iter_mut().chain(snap.src_pads.iter_mut());
                    for ip in pads_list {
                        if *ip.pad_name == *pad_name {
                            ip.caps = caps_str.clone();
                        }
                    }
                }
            }
        }
    }
}

/// Core CPU attribution logic, extracted for testability.
///
/// 1. Groups elements by their streaming thread ID.
/// 2. For each element with both sink and src pads, computes an approximate
///    processing time (`src.last_wall_ns - sink.last_wall_ns`).
/// 3. Within each thread group, distributes the thread's CPU usage
///    proportionally based on each element's processing time.  Because the
///    proportional fractions sum to 1.0, this consumes the full thread CPU
///    when any measured elements exist.
/// 4. Distributes residual CPU equally among unmeasured elements
///    (source-only, sink-only, tee) in each thread group.  The residual is
///    significant when **all** elements on a thread are unmeasured (e.g. a
///    source-only thread, or a queue+sink thread).  On mixed threads the
///    residual is ~0 due to the normalization in step 3.
///
/// Returns `(total_pipeline_cpu_pct, thread_groups)`.
fn attribute_cpu(
    element_snapshots: &mut FxHashMap<String, InternalElementSnapshot>,
    tracker: &ThreadCpuTracker,
) -> (f64, Vec<InternalThreadGroup>) {
    // Group elements by thread ID.
    let mut tid_to_elements: FxHashMap<u32, Vec<String>> =
        FxHashMap::with_capacity_and_hasher(element_snapshots.len(), Default::default());
    for (name, snap) in element_snapshots.iter() {
        if let Some(tid) = snap.thread_id {
            tid_to_elements.entry(tid).or_default().push(name.clone());
        }
    }

    // Per-element processing time is now computed causally at probe time
    // (src departure wall_ns − sink arrival wall_ns for each buffer) and
    // accumulated in ElementProbe. Read the mean from the snapshot.
    let mut element_processing_time: FxHashMap<String, f64> =
        FxHashMap::with_capacity_and_hasher(element_snapshots.len(), Default::default());
    for (name, snap) in element_snapshots.iter() {
        if let Some(pt) = snap.processing_time_us {
            element_processing_time.insert(name.clone(), pt);
        }
    }

    // Build thread groups and attribute CPU.
    let mut thread_groups = Vec::new();
    let mut total_pipeline_cpu = 0.0;

    for (tid, elements) in &tid_to_elements {
        let cpu_info = tracker.get(*tid);
        let thread_cpu_pct = cpu_info.map(|i| i.cpu_pct).unwrap_or(0.0);
        let thread_name = cpu_info.map(|i| i.thread_name.clone());

        total_pipeline_cpu += thread_cpu_pct;

        // Compute total processing time for this thread group.
        let total_proc_time: f64 = elements
            .iter()
            .filter_map(|name| element_processing_time.get(name))
            .sum();

        // Attribute CPU proportionally to measured elements.
        let mut attributed_cpu = 0.0;
        for name in elements {
            if let Some(&proc_time) = element_processing_time.get(name) {
                if let Some(snap) = element_snapshots.get_mut(name) {
                    if total_proc_time > 0.0 && thread_cpu_pct > 0.0 {
                        let fraction = proc_time / total_proc_time;
                        let element_cpu = fraction * thread_cpu_pct;
                        snap.estimated_cpu_pct = Some(element_cpu);
                        attributed_cpu += element_cpu;
                    }
                }
            }
        }

        // Residual attribution: distribute unaccounted CPU to unmeasured
        // elements (source-only, sink-only, tee) in this thread group.
        let unmeasured_count = elements
            .iter()
            .filter(|name| !element_processing_time.contains_key(*name))
            .count();

        if unmeasured_count > 0 && thread_cpu_pct > 0.0 {
            let residual = (thread_cpu_pct - attributed_cpu).max(0.0);
            let share = residual / unmeasured_count as f64;
            for name in elements {
                if !element_processing_time.contains_key(name) {
                    if let Some(snap) = element_snapshots.get_mut(name) {
                        snap.estimated_cpu_pct = Some(share);
                    }
                }
            }
        }

        let cpu_stats = tracker.cpu_history(*tid).and_then(|data| {
            if data.is_empty() {
                None
            } else {
                Some(SystemDistribution::from_slice(&data))
            }
        });

        thread_groups.push(InternalThreadGroup {
            thread_id: *tid,
            thread_name,
            cpu_pct: thread_cpu_pct,
            cpu_stats,
            elements: elements.clone(),
        });
    }

    (total_pipeline_cpu, thread_groups)
}

// Shared types (NodeInfo, TopologyEdge, PipelineTopology, RestartSnapshot,
// SystemSnapshot, SystemDistribution, EdgeDelay, PipelineSummary, PipelineSnapshot)
// are defined in `mcm_api::v1::stats` and re-exported above.

// ── System metrics buffer ──

pub struct SystemBuffer {
    capacity: usize,
    entries: VecDeque<SystemEntry>,
    generation: u64,
    cached: Option<(u64, SystemSnapshot)>,
}

struct SystemEntry {
    cpu_pct: f64,
    load_1m: f64,
    mem_used_pct: f64,
    temperature_c: f64,
}

impl SystemBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
            generation: 0,
            cached: None,
        }
    }

    pub fn record(&mut self, cpu_pct: f64, load_1m: f64, mem_used_pct: f64, temperature_c: f64) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(SystemEntry {
            cpu_pct,
            load_1m,
            mem_used_pct,
            temperature_c,
        });
        self.generation += 1;
    }

    /// Produce a system metrics snapshot.
    ///
    /// Uses online accumulators (Welford's algorithm) in a single pass over the
    /// entries, eliminating the 4 intermediate `Vec<f64>` allocations that the
    /// previous implementation required. Results are cached and reused until
    /// new samples arrive.
    pub fn snapshot(&mut self) -> SystemSnapshot {
        if let Some((gen, ref snap)) = self.cached {
            if gen == self.generation {
                return snap.clone();
            }
        }
        let snap = self.compute_snapshot();
        self.cached = Some((self.generation, snap.clone()));
        snap
    }

    fn compute_snapshot(&self) -> SystemSnapshot {
        if self.entries.is_empty() {
            return SystemSnapshot::default();
        }
        let mut cpu_acc = SystemDistributionAccumulator::new();
        let mut load_acc = SystemDistributionAccumulator::new();
        let mut mem_acc = SystemDistributionAccumulator::new();
        let mut temp_acc = SystemDistributionAccumulator::new();

        for entry in &self.entries {
            cpu_acc.push(entry.cpu_pct);
            load_acc.push(entry.load_1m);
            mem_acc.push(entry.mem_used_pct);
            temp_acc.push(entry.temperature_c);
        }

        let last = self.entries.back().unwrap();

        SystemSnapshot {
            sample_count: self.entries.len() as u64,
            current_cpu_pct: last.cpu_pct,
            current_load_1m: last.load_1m,
            current_mem_used_pct: last.mem_used_pct,
            current_temperature_c: last.temperature_c,
            cpu_stats: cpu_acc.finish(),
            load_stats: load_acc.finish(),
            mem_stats: mem_acc.finish(),
            temp_stats: temp_acc.finish(),
        }
    }

    pub fn reset(&mut self) {
        self.entries.clear();
        self.generation += 1;
    }
}

// SystemSnapshot, SystemDistribution, PipelineSnapshot, EdgeDelay, PipelineSummary
// are defined in `mcm_api::v1::stats` and re-exported above.

// ── Restart Tracker ──

struct RestartTracker {
    start_count: u64,
    first_start: Instant,
    last_start: Instant,
    restart_intervals: VecDeque<f64>,
}

impl RestartTracker {
    fn new() -> Self {
        Self {
            start_count: 1,
            first_start: Instant::now(),
            last_start: Instant::now(),
            restart_intervals: VecDeque::with_capacity(100),
        }
    }

    fn record_restart(&mut self) {
        let now = Instant::now();
        let interval = now.duration_since(self.last_start).as_secs_f64();
        self.restart_intervals.push_back(interval);
        if self.restart_intervals.len() > 100 {
            self.restart_intervals.pop_front();
        }
        self.last_start = now;
        self.start_count += 1;
    }

    fn snapshot(&self) -> RestartSnapshot {
        let now = Instant::now();
        let restart_count = self.start_count.saturating_sub(1);
        let avg = if !self.restart_intervals.is_empty() {
            self.restart_intervals.iter().sum::<f64>() / self.restart_intervals.len() as f64
        } else {
            0.0
        };
        let min = self
            .restart_intervals
            .iter()
            .copied()
            .fold(f64::MAX, f64::min);
        let last = self.restart_intervals.back().copied().unwrap_or(0.0);

        RestartSnapshot {
            start_count: self.start_count,
            restart_count,
            current_uptime_secs: now.duration_since(self.last_start).as_secs_f64(),
            total_tracked_secs: now.duration_since(self.first_start).as_secs_f64(),
            avg_restart_interval_secs: avg,
            min_restart_interval_secs: if min == f64::MAX { 0.0 } else { min },
            last_restart_interval_secs: last,
        }
    }
}

// ── Derived metrics computation ──

/// Get the most recent wall timestamp from pad snapshots,
/// preferring the `primary` direction with fallback to `secondary`.
fn latest_wall_ns(primary: &[InternalPadSnapshot], secondary: &[InternalPadSnapshot]) -> u64 {
    primary
        .iter()
        .chain(secondary.iter())
        .map(|p| p.last_wall_ns)
        .max()
        .unwrap_or(0)
}

/// Get the pad snapshot with the most data, preferring srcs over sinks.
fn primary_pad(snap: &InternalElementSnapshot) -> Option<&InternalPadSnapshot> {
    snap.src_pads
        .iter()
        .chain(snap.sink_pads.iter())
        .max_by_key(|p| p.total_buffers)
}

/// Per-pad metrics derived from a single pass over the records.
///
/// Computes unique PTS frame count, wall-clock time span, and throughput
/// from one HashSet construction instead of three.
struct PadMetrics {
    frame_count: u64,
    unique_pts_count: usize,
    throughput_fps: Option<f64>,
}

fn pad_metrics(pad: &InternalPadSnapshot, pts_scratch: &mut Vec<u64>) -> PadMetrics {
    if let Some(records) = &pad.records {
        pts_scratch.clear();
        let pts_vals = pts_scratch;
        let mut first_wall: u64 = u64::MAX;
        let mut last_wall: u64 = 0;
        for record in records.iter() {
            if record.wall_ns < first_wall {
                first_wall = record.wall_ns;
            }
            if record.wall_ns > last_wall {
                last_wall = record.wall_ns;
            }
            if let Some(pts) = record.pts_ns {
                pts_vals.push(pts);
            }
        }

        if !pts_vals.is_sorted() {
            pts_vals.sort_unstable();
        }
        pts_vals.dedup();
        let unique_pts_count = pts_vals.len();

        let frame_count = if unique_pts_count == 0 {
            pad.total_buffers
        } else {
            unique_pts_count as u64
        };

        let throughput_fps = if unique_pts_count >= 2 {
            let duration_ns = last_wall.saturating_sub(first_wall);
            if duration_ns > 0 {
                Some(unique_pts_count as f64 / (duration_ns as f64 / 1e9))
            } else {
                None
            }
        } else {
            None
        };

        return PadMetrics {
            frame_count,
            unique_pts_count,
            throughput_fps,
        };
    }

    // Lite mode fallback: derive throughput from accumulators or distributions.
    let throughput_fps = match &pad.accumulators {
        Some(acc) if acc.mean_interval_ms > 0.0 => Some(1000.0 / acc.mean_interval_ms),
        _ => pad
            .distribution
            .as_ref()
            .filter(|d| d.interval.mean > 0.0)
            .map(|d| 1000.0 / d.interval.mean),
    };

    PadMetrics {
        frame_count: pad.total_buffers,
        unique_pts_count: 0,
        throughput_fps,
    }
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[mid - 1] + values[mid]) / 2.0)
    } else {
        Some(values[mid])
    }
}

/// Reusable scratch buffers for PTS merge-join operations, avoiding repeated
/// Vec allocations across multiple edge/element computations per snapshot.
struct MergeJoinScratch {
    sink_pairs: Vec<(u64, u64)>,
    result_values: Vec<f64>,
    pts_scratch: Vec<u64>,
}

impl MergeJoinScratch {
    fn new() -> Self {
        Self {
            sink_pairs: Vec::with_capacity(1024),
            result_values: Vec::with_capacity(1024),
            pts_scratch: Vec::with_capacity(1024),
        }
    }
}

/// Compute inter-element freshness deltas from the topology graph edges.
///
/// For each edge in the topology, compute the wall-clock delta between the
/// source element's src pad timestamp and the destination element's sink pad
/// timestamp.
fn compute_edge_delays(
    elements: &FxHashMap<String, InternalElementSnapshot>,
    topo: Option<&PipelineTopology>,
    scratch: &mut MergeJoinScratch,
) -> Vec<InternalEdgeDelay> {
    let topo = match topo {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut delays = Vec::with_capacity(topo.edges.len());
    for (i, edge) in topo.edges.iter().enumerate() {
        if let (Some(from_snap), Some(to_snap)) =
            (elements.get(&edge.from_node), elements.get(&edge.to_node))
        {
            let from_wall = latest_wall_ns(&from_snap.src_pads, &from_snap.sink_pads);
            let to_wall = latest_wall_ns(&to_snap.sink_pads, &to_snap.src_pads);
            if from_wall > 0 && to_wall > 0 {
                let delay_ms = (to_wall.saturating_sub(from_wall)) as f64 / 1e6;
                let (
                    causal_latency_ms,
                    causal_match_rate,
                    causal_matched_samples,
                    causal_confidence,
                ) = compute_causal_edge_latency(
                    from_snap,
                    to_snap,
                    &edge.from_pad,
                    &edge.to_pad,
                    scratch,
                );
                delays.push(InternalEdgeDelay {
                    edge_idx: i,
                    freshness_delay_ms: delay_ms,
                    causal_latency_ms,
                    causal_match_rate,
                    causal_matched_samples,
                    causal_confidence,
                });
            }
        }
    }

    delays
}

/// Compute causal edge latency using exact PTS matching across linked pads.
/// Returns `(distribution_ms, match_rate, matched_samples, confidence)`.
fn compute_causal_edge_latency(
    from_snap: &InternalElementSnapshot,
    to_snap: &InternalElementSnapshot,
    from_pad_name: &str,
    to_pad_name: &str,
    scratch: &mut MergeJoinScratch,
) -> (
    Option<mcm_api::v1::stats::Distribution>,
    Option<f64>,
    Option<u64>,
    Option<CausalConfidence>,
) {
    let from_pad = from_snap
        .src_pads
        .iter()
        .find(|p| *p.pad_name == *from_pad_name)
        .or_else(|| from_snap.src_pads.iter().max_by_key(|p| p.total_buffers));

    // Tee fallback: when src pads are empty (tee elements with skip_src_pads),
    // use the tee's sink pad as the upstream PTS reference. PTS is copied
    // identically from tee sink to all src pads, so PTS matching is valid.
    let from_pad = from_pad.or_else(|| {
        if from_snap.src_pads.is_empty() {
            from_snap.sink_pads.iter().max_by_key(|p| p.total_buffers)
        } else {
            None
        }
    });

    let to_pad = to_snap
        .sink_pads
        .iter()
        .find(|p| *p.pad_name == *to_pad_name)
        .or_else(|| to_snap.sink_pads.iter().max_by_key(|p| p.total_buffers));

    let (Some(from_pad), Some(to_pad)) = (from_pad, to_pad) else {
        return (None, None, None, None);
    };
    let (Some(from_records), Some(to_records)) = (&from_pad.records, &to_pad.records) else {
        return (None, None, None, None);
    };

    scratch.sink_pairs.clear();
    for r in to_records.iter() {
        if let Some(pts) = r.pts_ns {
            scratch.sink_pairs.push((pts, r.wall_ns));
        }
    }
    if scratch.sink_pairs.is_empty() {
        return (None, Some(0.0), Some(0), Some(CausalConfidence::Low));
    }
    if !scratch.sink_pairs.is_sorted_by_key(|&(pts, _)| pts) {
        scratch.sink_pairs.sort_unstable();
    }

    scratch.result_values.clear();
    let sink_pairs = scratch.sink_pairs.as_slice();
    let sink_len = sink_pairs.len();
    let mut src_with_pts: u64 = 0;
    let mut cursor: usize = 0;

    for r in from_records.iter() {
        let Some(pts) = r.pts_ns else {
            continue;
        };
        src_with_pts += 1;

        // SAFETY: every `sink_pairs[idx]` below is guarded by `idx < sink_len`
        // (which equals `sink_pairs.len()`). Using `get_unchecked` removes the
        // redundant bounds checks that the compiler can't elide through the
        // complex control flow (~5% of total CPU in the profiled hot path).
        unsafe {
            if cursor < sink_len && sink_pairs.get_unchecked(cursor).0 == pts {
                if cursor + 1 >= sink_len || sink_pairs.get_unchecked(cursor + 1).0 != pts {
                    if sink_pairs.get_unchecked(cursor).1 >= r.wall_ns {
                        scratch
                            .result_values
                            .push((sink_pairs.get_unchecked(cursor).1 - r.wall_ns) as f64 / 1e6);
                        cursor += 1;
                    }
                    continue;
                }
            } else {
                if cursor < sink_len && sink_pairs.get_unchecked(cursor).0 < pts {
                    let next = cursor + 1;
                    if next < sink_len && sink_pairs.get_unchecked(next).0 >= pts {
                        cursor = next;
                    } else {
                        cursor += sink_pairs[cursor..].partition_point(|&(p, _)| p < pts);
                    }
                } else if cursor < sink_len && sink_pairs.get_unchecked(cursor).0 > pts {
                    cursor = sink_pairs[..cursor].partition_point(|&(p, _)| p < pts);
                }

                if cursor >= sink_len || sink_pairs.get_unchecked(cursor).0 != pts {
                    continue;
                }

                if cursor + 1 >= sink_len || sink_pairs.get_unchecked(cursor + 1).0 != pts {
                    if sink_pairs.get_unchecked(cursor).1 >= r.wall_ns {
                        scratch
                            .result_values
                            .push((sink_pairs.get_unchecked(cursor).1 - r.wall_ns) as f64 / 1e6);
                        cursor += 1;
                    }
                    continue;
                }
            }

            let group_start = cursor;
            let mut group_end = group_start + 1;
            while group_end < sink_len && sink_pairs.get_unchecked(group_end).0 == pts {
                group_end += 1;
            }
            let rel = sink_pairs[group_start..group_end].partition_point(|&(_, w)| w < r.wall_ns);
            let idx = group_start + rel;
            if idx < group_end && sink_pairs.get_unchecked(idx).1 >= r.wall_ns {
                scratch
                    .result_values
                    .push((sink_pairs.get_unchecked(idx).1 - r.wall_ns) as f64 / 1e6);
                cursor = idx + 1;
            }
        }
    }

    let matched = scratch.result_values.len() as u64;
    let match_rate = if src_with_pts > 0 {
        matched as f64 / src_with_pts as f64
    } else {
        0.0
    };
    let distribution = if scratch.result_values.is_empty() {
        None
    } else {
        Some(mcm_api::v1::stats::Distribution::from_slice(
            &scratch.result_values,
        ))
    };
    let confidence = Some(causal_confidence_label(match_rate, matched));

    (distribution, Some(match_rate), Some(matched), confidence)
}

/// Compute PTS-matched intra-element processing time for all elements that
/// have Full-mode records on both their sink and src pads. For each matched
/// PTS, computes `src_wall_ns − sink_wall_ns` to get the per-buffer transit
/// time in microseconds.
///
/// For single-threaded elements this equals processing time; for cross-thread
/// elements (queues) it equals queuing delay + processing time — which is the
/// true metric users care about.
///
/// When a distribution is computed, it replaces the atomic-based mean in
/// `processing_time_us` with the distribution's mean and populates
/// `processing_time_stats` with the full distribution.
fn compute_intra_element_processing_time(
    elements: &mut FxHashMap<String, InternalElementSnapshot>,
    scratch: &mut MergeJoinScratch,
) {
    for snap in elements.values_mut() {
        let sink_records = snap
            .sink_pads
            .iter()
            .filter_map(|p| p.records.as_ref())
            .max_by_key(|r| r.len());
        let src_records = snap
            .src_pads
            .iter()
            .filter_map(|p| p.records.as_ref())
            .max_by_key(|r| r.len());

        let (Some(sink_recs), Some(src_recs)) = (sink_records, src_records) else {
            continue;
        };
        if sink_recs.is_empty() || src_recs.is_empty() {
            continue;
        }

        scratch.sink_pairs.clear();
        for r in sink_recs.iter() {
            if let Some(pts) = r.pts_ns {
                scratch.sink_pairs.push((pts, r.wall_ns));
            }
        }
        if scratch.sink_pairs.is_empty() {
            continue;
        }
        if !scratch.sink_pairs.is_sorted_by_key(|&(pts, _)| pts) {
            scratch.sink_pairs.sort_unstable();
        }

        scratch.result_values.clear();
        let sink_pairs = scratch.sink_pairs.as_slice();
        let sink_len = sink_pairs.len();
        let mut cursor: usize = 0;

        for r in src_recs.iter() {
            let Some(pts) = r.pts_ns else { continue };

            // SAFETY: every access is guarded by `idx < sink_len`.
            unsafe {
                if cursor < sink_len && sink_pairs.get_unchecked(cursor).0 == pts {
                    if cursor + 1 >= sink_len || sink_pairs.get_unchecked(cursor + 1).0 != pts {
                        if sink_pairs.get_unchecked(cursor).1 <= r.wall_ns {
                            let delta_us =
                                (r.wall_ns - sink_pairs.get_unchecked(cursor).1) as f64 / 1_000.0;
                            scratch.result_values.push(delta_us);
                        }
                        continue;
                    }
                } else {
                    if cursor < sink_len && sink_pairs.get_unchecked(cursor).0 < pts {
                        let next = cursor + 1;
                        if next < sink_len && sink_pairs.get_unchecked(next).0 >= pts {
                            cursor = next;
                        } else {
                            cursor += sink_pairs[cursor..].partition_point(|&(p, _)| p < pts);
                        }
                    } else if cursor < sink_len && sink_pairs.get_unchecked(cursor).0 > pts {
                        cursor = sink_pairs[..cursor].partition_point(|&(p, _)| p < pts);
                    }

                    if cursor >= sink_len || sink_pairs.get_unchecked(cursor).0 != pts {
                        continue;
                    }

                    if cursor + 1 >= sink_len || sink_pairs.get_unchecked(cursor + 1).0 != pts {
                        if sink_pairs.get_unchecked(cursor).1 <= r.wall_ns {
                            let delta_us =
                                (r.wall_ns - sink_pairs.get_unchecked(cursor).1) as f64 / 1_000.0;
                            scratch.result_values.push(delta_us);
                        }
                        continue;
                    }
                }

                let group_start = cursor;
                let mut group_end = group_start + 1;
                while group_end < sink_len && sink_pairs.get_unchecked(group_end).0 == pts {
                    group_end += 1;
                }
                let rel =
                    sink_pairs[group_start..group_end].partition_point(|&(_, w)| w <= r.wall_ns);
                if rel > 0 {
                    let sink_wall = sink_pairs.get_unchecked(group_start + rel - 1).1;
                    let delta_us = (r.wall_ns - sink_wall) as f64 / 1_000.0;
                    scratch.result_values.push(delta_us);
                }
            }
        }

        if scratch.result_values.is_empty() {
            continue;
        }

        let dist = mcm_api::v1::stats::Distribution::compute(&mut scratch.result_values);
        snap.processing_time_us = Some(dist.mean);
        snap.processing_time_stats = Some(dist);
    }
}

fn causal_confidence_label(match_rate: f64, matched_samples: u64) -> CausalConfidence {
    if matched_samples >= 50 && match_rate >= 0.8 {
        CausalConfidence::High
    } else if matched_samples >= 20 && match_rate >= 0.4 {
        CausalConfidence::Medium
    } else {
        CausalConfidence::Low
    }
}

fn aggregate_causal_latency_health(edges: &[InternalEdgeDelay]) -> Option<CausalConfidence> {
    let mut weighted_score = 0.0;
    let mut total_weight = 0.0;

    for edge in edges {
        let Some(confidence) = edge.causal_confidence else {
            continue;
        };
        let weight = edge.causal_matched_samples.unwrap_or(1).max(1) as f64;
        let score = match confidence {
            CausalConfidence::Low => 1.0,
            CausalConfidence::Medium => 2.0,
            CausalConfidence::High => 3.0,
        };
        weighted_score += score * weight;
        total_weight += weight;
    }

    if total_weight <= 0.0 {
        return None;
    }

    let avg = weighted_score / total_weight;
    if avg >= 2.5 {
        Some(CausalConfidence::High)
    } else if avg >= 1.5 {
        Some(CausalConfidence::Medium)
    } else {
        Some(CausalConfidence::Low)
    }
}

/// Compute aggregate pipeline summary from element snapshots.
///
/// Finds the most-active element (highest buffer count) for throughput,
/// and sums all positive edge freshness deltas for total pipeline freshness.
fn compute_summary(
    elements: &FxHashMap<String, InternalElementSnapshot>,
    edges: &[InternalEdgeDelay],
    _topo: Option<&PipelineTopology>,
    pts_scratch: &mut Vec<u64>,
) -> PipelineSummary {
    let mut all_metrics = Vec::with_capacity(elements.len());
    for elem in elements.values() {
        if let Some(pad) = primary_pad(elem) {
            all_metrics.push(pad_metrics(pad, pts_scratch));
        }
    }

    let total_frames = all_metrics.iter().map(|m| m.frame_count).max().unwrap_or(0);

    // Prefer throughput derived from unique PTS (frame cadence), then robustly
    // aggregate fallbacks while filtering obviously packet-level rates.
    let mut pts_candidates = Vec::with_capacity(all_metrics.len());
    let mut fallback_candidates = Vec::with_capacity(all_metrics.len());
    for m in &all_metrics {
        if let Some(fps) = m.throughput_fps {
            if fps.is_finite() && fps > 0.1 && fps <= 240.0 {
                fallback_candidates.push(fps);
                if m.unique_pts_count >= 2 {
                    pts_candidates.push(fps);
                }
            }
        }
    }

    let throughput = median(&mut pts_candidates)
        .or_else(|| median(&mut fallback_candidates))
        .unwrap_or(0.0);

    // Total pipeline freshness delta: sum all positive edge deltas.
    let total_delay: f64 = edges.iter().map(|e| e.freshness_delay_ms.max(0.0)).sum();
    let causal_latency_health = aggregate_causal_latency_health(edges);

    // Sum causal latency across edges that have data
    let total_pipeline_causal_latency_ms = {
        let mut causal_edges = Vec::with_capacity(edges.len());
        for e in edges {
            if e.causal_latency_ms.is_some() {
                causal_edges.push(e);
            }
        }

        if causal_edges.is_empty() {
            None
        } else {
            let sum_mean: f64 = causal_edges
                .iter()
                .filter_map(|e| e.causal_latency_ms.as_ref())
                .map(|d| d.mean)
                .sum();
            let sum_p95: f64 = causal_edges
                .iter()
                .filter_map(|e| e.causal_latency_ms.as_ref())
                .map(|d| d.p95)
                .sum();
            let sum_p99: f64 = causal_edges
                .iter()
                .filter_map(|e| e.causal_latency_ms.as_ref())
                .map(|d| d.p99)
                .sum();
            let sum_min: f64 = causal_edges
                .iter()
                .filter_map(|e| e.causal_latency_ms.as_ref())
                .map(|d| d.min)
                .sum();
            let sum_max: f64 = causal_edges
                .iter()
                .filter_map(|e| e.causal_latency_ms.as_ref())
                .map(|d| d.max)
                .sum();
            let min_count: u64 = causal_edges
                .iter()
                .filter_map(|e| e.causal_latency_ms.as_ref())
                .map(|d| d.count)
                .min()
                .unwrap_or(0);

            Some(mcm_api::v1::stats::Distribution {
                count: min_count,
                min: sum_min,
                max: sum_max,
                mean: sum_mean,
                std: 0.0,         // std of a sum requires covariance info we don't have
                median: sum_mean, // best approximation without per-sample data
                p95: sum_p95,
                p99: sum_p99,
            })
        }
    };

    let verdict = if total_frames == 0 {
        "No data".to_string()
    } else if total_delay > 10.0 {
        format!("High pipeline freshness delta ({:.1}ms)", total_delay)
    } else {
        format!(
            "OK ({} frames, {:.1}fps, {:.1}ms freshness delta)",
            total_frames, throughput, total_delay
        )
    };

    PipelineSummary {
        total_frames,
        throughput_fps: throughput,
        total_pipeline_freshness_delay_ms: total_delay,
        total_pipeline_causal_latency_ms,
        causal_latency_health,
        verdict,
    }
}

// ── Helpers ──

/// Install a `BUFFER | BUFFER_LIST` probe on a single GStreamer pad.
///
/// The probe closure captures the `PadBuffer` for data recording and the
/// `ElementProbe` for thread ID tracking and per-buffer processing time.
/// On each buffer, it:
/// 1. Samples the wall clock once.
/// 2. Records the buffer observation (wall time, size, keyframe flag).
/// 3. For **sink** pads: stores the wall time in the element's
///    `last_sink_arrival_ns` (start of in-element processing).
///    For **src** pads: computes `wall_ns − last_sink_arrival_ns` and
///    accumulates it as per-buffer causal processing time.
/// 4. Updates the element's streaming thread ID via `gettid()` (Linux only).
fn install_pad_probe(
    pad: &gst::Pad,
    pad_buf: &Arc<PadBuffer>,
    analysis: &Arc<PipelineAnalysis>,
    element_probe: &Arc<ElementProbe>,
    direction: gst::PadDirection,
) {
    let pb = Arc::clone(pad_buf);
    let enabled = Arc::clone(analysis);
    let ep = Arc::clone(element_probe);
    let is_src = direction == gst::PadDirection::Src;
    pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
        move |_pad, info| {
            if !enabled.is_enabled() {
                return gst::PadProbeReturn::Ok;
            }
            let wall_ns = wall_clock_ns();
            record_probe_data(&pb, &info.data, wall_ns);
            if is_src {
                ep.record_src_departure(wall_ns);
            } else {
                ep.record_sink_arrival(wall_ns);
            }
            ep.update_thread_id(is_src);
            gst::PadProbeReturn::Ok
        },
    );
}

/// Record a probe observation from either a single Buffer or a BufferList.
///
/// For **Buffer**: records one entry (size, keyframe flag).
///
/// For **BufferList**: records **one aggregate entry** per list using the total
/// payload size and the keyframe flag from the first buffer. This preserves
/// frame-level measurement granularity even after payloaders that split a single
/// frame into many RTP packets (e.g. `rtph264pay` in `aggregate-mode=zero-latency`).
#[inline]
fn record_probe_data(pad_buf: &PadBuffer, data: &Option<gst::PadProbeData>, wall_ns: u64) {
    match data {
        Some(gst::PadProbeData::Buffer(ref buffer)) => {
            let size = buffer.size() as u32;
            let is_kf = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT);
            let pts_ns = buffer.pts().map(|t| t.nseconds());
            pad_buf.record(wall_ns, pts_ns, size, is_kf);
        }
        Some(gst::PadProbeData::BufferList(ref list)) => {
            // Aggregate: total size of all buffers in the list, keyframe flag
            // from the first buffer (the list represents one logical unit).
            let total_size = list.calculate_size() as u32;
            let is_kf = list
                .get(0)
                .map(|b| !b.flags().contains(gst::BufferFlags::DELTA_UNIT))
                .unwrap_or(false);
            let pts_ns = list.get(0).and_then(|b| b.pts()).map(|t| t.nseconds());
            pad_buf.record(wall_ns, pts_ns, total_size, is_kf);
        }
        _ => {}
    }
}

/// Get the current wall-clock time in nanoseconds since the Unix epoch.
/// On Pi4 ARMv7 this is a real syscall (~716ns). Call once per probe callback.
#[inline]
fn wall_clock_ns() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // CLOCK_REALTIME_COARSE is vDSO-accelerated on ARM (~23ns vs ~716ns
        // for CLOCK_REALTIME). Resolution is ~1-4ms (CONFIG_HZ dependent),
        // which is adequate for 30fps video (33ms intervals): mean over 900
        // ring samples converges to <0.4% error, and distribution percentiles
        // use rank order so quantization doesn't affect them.
        unsafe { libc::clock_gettime(libc::CLOCK_REALTIME_COARSE, &mut ts) };
        (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

// ── Conversion: internal types → public API types ──

/// Populate `PipelineConnection`s by matching proxy/shm bridge elements across
/// pipelines within the same stream. For each `proxysink{N}` found in pipeline A,
/// locates the `proxysrc{N}` in pipeline B and adds a bidirectional connection.
/// Recursively visit all elements in a pipeline's bin tree.
fn for_each_element<F: FnMut(&ElementSnapshot)>(pipeline: &PipelineSnapshot, mut f: F) {
    fn visit(elements: &[ElementSnapshot], f: &mut impl FnMut(&ElementSnapshot)) {
        for elem in elements {
            f(elem);
            visit(&elem.children, f);
        }
    }
    visit(&pipeline.elements, &mut f);
}

fn derive_pipeline_connections(pipelines: &mut [PipelineSnapshot]) {
    let total_elements: usize = pipelines
        .iter()
        .map(|p| {
            let mut count = 0usize;
            for_each_element(p, |_| count += 1);
            count
        })
        .sum();
    let mut element_pipeline: FxHashMap<String, usize> =
        FxHashMap::with_capacity_and_hasher(total_elements, Default::default());
    for (pi, pipeline) in pipelines.iter().enumerate() {
        for_each_element(pipeline, |elem| {
            element_pipeline.insert(elem.name.clone(), pi);
        });
    }

    let mut connections: Vec<(usize, PipelineConnection)> = Vec::new();
    for (pi, pipeline) in pipelines.iter().enumerate() {
        for_each_element(pipeline, |elem| {
            let (peer_prefix, bridge_type) = if elem.element_type == "proxysink" {
                ("proxysrc", "proxy")
            } else if elem.element_type == "shmsink" {
                ("shmsrc", "shm")
            } else {
                return;
            };

            let suffix = elem.name.strip_prefix(&elem.element_type).unwrap_or("");
            let peer_name = format!("{peer_prefix}{suffix}");

            if let Some(&target_pi) = element_pipeline.get(&peer_name) {
                if target_pi != pi {
                    connections.push((
                        pi,
                        PipelineConnection {
                            to_pipeline: pipelines[target_pi].name.clone(),
                            bridge_type: bridge_type.to_string(),
                            from_element: elem.name.clone(),
                            to_element: peer_name,
                        },
                    ));
                }
            }
        });
    }

    for (pi, conn) in connections {
        pipelines[pi].connections.push(conn);
    }
}

/// Convert an `InternalPipelineData` into the public `PipelineSnapshot` API type.
///
/// Takes a borrow to avoid deep-cloning the entire internal snapshot (which
/// includes ~36K RawRecords per pipeline). Individual fields are cloned at
/// the API output boundary where ownership is required.
fn build_api_pipeline_snapshot(
    ipd: &InternalPipelineData,
    buffer_limit: usize,
) -> PipelineSnapshot {
    // Build edge delay index: from_element -> Vec<ElementConnection>
    let mut edge_index: FxHashMap<String, Vec<ElementConnection>> =
        FxHashMap::with_capacity_and_hasher(ipd.edges.len(), Default::default());
    let topo_edges: &[TopologyEdge] = ipd
        .topology
        .as_ref()
        .map(|t| t.edges.as_slice())
        .unwrap_or(&[]);

    for edge in &ipd.edges {
        let topo_edge = &topo_edges[edge.edge_idx];
        edge_index
            .entry(topo_edge.from_node.clone())
            .or_default()
            .push(ElementConnection {
                to_element: topo_edge.to_node.clone(),
                freshness_delay_ms: edge.freshness_delay_ms,
                causal_latency_ms: edge.causal_latency_ms.clone(),
                causal_match_rate: edge.causal_match_rate,
                causal_matched_samples: edge.causal_matched_samples,
                causal_confidence: edge.causal_confidence,
            });
    }

    // Build element→thread_id lookup for thread connection detection.
    let elem_thread_map: FxHashMap<&str, u32> = ipd
        .elements
        .iter()
        .filter_map(|(name, e)| e.thread_id.map(|tid| (name.as_str(), tid)))
        .collect();

    // Detect thread connections via queue elements that bridge two threads.
    // A queue's sink pad runs on the upstream (pushing) thread, while its src
    // pad runs on a new (pulling) thread.  We check both directions:
    //   upstream_thread → queue_thread  (edge TO queue)
    //   queue_thread → downstream_thread  (edge FROM queue, if different)
    let mut thread_conn_set: FxHashSet<(u32, u32, String)> = FxHashSet::default();
    if let Some(ref topo) = ipd.topology {
        for (name, elem) in &ipd.elements {
            let is_queue =
                elem.element_type.contains("queue") || elem.element_type.contains("Queue");
            if !is_queue {
                continue;
            }
            let Some(queue_tid) = elem.thread_id else {
                continue;
            };

            for te in &topo.edges {
                // Upstream → queue: the pushing thread connects to queue's thread.
                if te.to_node == *name {
                    if let Some(&upstream_tid) = elem_thread_map.get(te.from_node.as_str()) {
                        if upstream_tid != queue_tid {
                            thread_conn_set.insert((upstream_tid, queue_tid, name.clone()));
                        }
                    }
                }
                // Queue → downstream: if the downstream is on yet another thread.
                if te.from_node == *name {
                    if let Some(&downstream_tid) = elem_thread_map.get(te.to_node.as_str()) {
                        if downstream_tid != queue_tid {
                            thread_conn_set.insert((queue_tid, downstream_tid, name.clone()));
                        }
                    }
                }
            }
        }
    }
    let mut thread_conn_map: FxHashMap<u32, Vec<ThreadConnection>> = FxHashMap::default();
    for (from_tid, to_tid, via) in thread_conn_set {
        thread_conn_map
            .entry(from_tid)
            .or_default()
            .push(ThreadConnection {
                to_thread: to_tid,
                via_element: via,
            });
    }

    // Compute root cause candidates and thread bottlenecks
    let root_cause_candidates = root_cause::root_causes_for_internal(
        &ipd.elements,
        &ipd.edges,
        ipd.expected_interval_ms,
        ipd.pipeline_cpu_pct,
    );
    let thread_bottlenecks =
        root_cause::thread_bottlenecks_for_internal(&ipd.thread_groups, &ipd.summary);

    // Group elements by parent_bin → build recursive hierarchy.
    // Key: parent bin name (None = direct pipeline child).
    let mut children_of: FxHashMap<Option<&str>, Vec<&InternalElementSnapshot>> =
        FxHashMap::with_capacity_and_hasher(ipd.elements.len(), Default::default());
    for (_name, elem) in &ipd.elements {
        let parent = elem.parent_bin.as_deref();
        children_of.entry(parent).or_default().push(elem);
    }

    // Identify which elements are bins (present as parent_bin of other elements,
    // or flagged in topology).
    let topo_bins: FxHashSet<&str> = ipd
        .topology
        .as_ref()
        .map(|t| {
            t.nodes
                .iter()
                .filter(|n| n.is_bin)
                .map(|n| n.name.as_str())
                .collect()
        })
        .unwrap_or_default();
    let parent_refs: FxHashSet<&str> = ipd
        .elements
        .values()
        .filter_map(|e| e.parent_bin.as_deref())
        .collect();
    let all_bins: FxHashSet<&str> = topo_bins.union(&parent_refs).copied().collect();

    // Recursive builder. All elements (including bins) become ElementSnapshot.
    // Bins get `is_bin = true` and their children nested in `children`.
    fn build_children(
        bin_name: Option<&str>,
        children_of: &FxHashMap<Option<&str>, Vec<&InternalElementSnapshot>>,
        all_bins: &FxHashSet<&str>,
        edge_index: &mut FxHashMap<String, Vec<ElementConnection>>,
        topo_edges: &[TopologyEdge],
        expected_interval_ms: f64,
        buffer_limit: usize,
        element_cpu_history: &FxHashMap<String, Vec<f64>>,
    ) -> Vec<ElementSnapshot> {
        let children = match children_of.get(&bin_name) {
            Some(c) => c,
            None => return Vec::new(),
        };
        let mut elements = Vec::with_capacity(children.len());
        for ie in children {
            let mut elem = convert_element(
                ie,
                edge_index,
                topo_edges,
                expected_interval_ms,
                buffer_limit,
                element_cpu_history,
            );
            if all_bins.contains(&*ie.element_name) {
                elem.is_bin = true;
                elem.children = build_children(
                    Some(&ie.element_name),
                    children_of,
                    all_bins,
                    edge_index,
                    topo_edges,
                    expected_interval_ms,
                    buffer_limit,
                    element_cpu_history,
                );
            }
            elements.push(elem);
        }
        elements
    }

    let mut pipeline_elements = build_children(
        None,
        &children_of,
        &all_bins,
        &mut edge_index,
        topo_edges,
        ipd.expected_interval_ms,
        buffer_limit,
        &ipd.element_cpu_history,
    );

    // Build flat thread summaries (no nested elements).
    let mut threads: Vec<ThreadSummary> = Vec::with_capacity(ipd.thread_groups.len());
    for tg in &ipd.thread_groups {
        let connections = thread_conn_map.remove(&tg.thread_id).unwrap_or_default();
        threads.push(ThreadSummary {
            id: tg.thread_id,
            name: tg.thread_name.clone(),
            stats: ThreadStats {
                name: tg.thread_name.clone(),
                cpu_pct: tg.cpu_pct,
                cpu_stats: tg.cpu_stats.clone(),
            },
            connections,
        });
    }

    // Add stub pads for topology-known pads that weren't captured by probes
    // (e.g. tee dynamic src pads, which are skipped during probing).
    if !topo_edges.is_empty() {
        add_stub_pads_recursive(&mut pipeline_elements, topo_edges);
    }

    let mut snapshot = PipelineSnapshot {
        name: ipd.name.clone(),
        stats: PipelineStats {
            level: ipd.stats_level,
            window_size: ipd.window_size,
            expected_interval_ms: ipd.expected_interval_ms,
            uptime_secs: ipd.uptime_secs,
            health: HealthStatus::Unknown,
            dominant_issue: IssueKind::Unknown,
            cpu_pct: ipd.pipeline_cpu_pct,
            cpu_stats: ipd.pipeline_cpu_stats.clone(),
            summary: ipd.summary.clone(),
            system: ipd.system.clone(),
            restarts: ipd.restarts.clone(),
            root_cause_candidates,
            thread_bottlenecks,
        },
        connections: Vec::new(),
        elements: pipeline_elements,
        threads,
    };

    let (h, i) = health::pipeline_health(&snapshot);
    snapshot.stats.health = h;
    snapshot.stats.dominant_issue = i;

    snapshot
}

/// Recursively add stub pads to elements and their children.
fn add_stub_pads_recursive(elements: &mut [ElementSnapshot], topo_edges: &[TopologyEdge]) {
    for elem in elements {
        for te in topo_edges {
            let (pad_name, direction) = if te.from_node == elem.name {
                (&te.from_pad, PadDirection::Src)
            } else if te.to_node == elem.name {
                (&te.to_pad, PadDirection::Sink)
            } else {
                continue;
            };
            if elem.pads.iter().any(|p| p.name == *pad_name) {
                continue;
            }

            let mut connections = Vec::new();
            for e in topo_edges {
                match direction {
                    PadDirection::Src
                        if e.from_node == elem.name && e.from_pad == *pad_name =>
                    {
                        connections.push(PadConnection {
                            peer_element: e.to_node.clone(),
                            peer_pad: e.to_pad.clone(),
                            media_type: e.media_type.clone(),
                        });
                    }
                    PadDirection::Sink
                        if e.to_node == elem.name && e.to_pad == *pad_name =>
                    {
                        connections.push(PadConnection {
                            peer_element: e.from_node.clone(),
                            peer_pad: e.from_pad.clone(),
                            media_type: e.media_type.clone(),
                        });
                    }
                    _ => {}
                }
            }

            elem.pads.push(PadSnapshot {
                name: pad_name.clone(),
                direction,
                caps: None,
                stats: PadStats {
                    level: StatsLevel::Lite,
                    total_buffers: 0,
                    total_keyframes: 0,
                    total_delta_frames: 0,
                    total_dropped: 0,
                    drop_ratio: 0.0,
                    bitrate_bps: None,
                    avg_gop_size: None,
                    keyframe_interval_ms: None,
                    last_wall_ns: 0,
                    accumulators: None,
                    distribution: None,
                },
                buffer: Vec::new(),
                connections,
            });
        }
        add_stub_pads_recursive(&mut elem.children, topo_edges);
    }
}

/// Convert an `InternalElementSnapshot` to the public `ElementSnapshot` API type.
///
/// Takes a borrow to avoid cloning the element's raw records (~21KB per pad).
fn convert_element(
    ie: &InternalElementSnapshot,
    edge_index: &mut FxHashMap<String, Vec<ElementConnection>>,
    topo_edges: &[TopologyEdge],
    expected_interval_ms: f64,
    buffer_limit: usize,
    element_cpu_history: &FxHashMap<String, Vec<f64>>,
) -> ElementSnapshot {
    let connections = edge_index.remove(&*ie.element_name).unwrap_or_default();

    let (health, stutter, freeze, max_freeze, ratio) =
        diagnostics::element_diagnostics_from_internal(ie, expected_interval_ms);

    let mut pads = Vec::with_capacity(ie.sink_pads.len() + ie.src_pads.len());
    for pad in &ie.sink_pads {
        pads.push(convert_pad(
            pad,
            PadDirection::Sink,
            &ie.element_name,
            topo_edges,
            expected_interval_ms,
            buffer_limit,
        ));
    }
    for pad in &ie.src_pads {
        pads.push(convert_pad(
            pad,
            PadDirection::Src,
            &ie.element_name,
            topo_edges,
            expected_interval_ms,
            buffer_limit,
        ));
    }

    let queue_stats = ie.queue_stats.as_ref().map(|qs| {
        let fill_pct = if qs.max_level_buffers > 0 {
            qs.current_level_buffers as f64 / qs.max_level_buffers as f64 * 100.0
        } else if qs.max_level_bytes > 0 {
            qs.current_level_bytes as f64 / qs.max_level_bytes as f64 * 100.0
        } else {
            0.0
        };
        mcm_api::v1::stats::QueueStats {
            current_level_buffers: qs.current_level_buffers,
            current_level_bytes: qs.current_level_bytes,
            current_level_time_ns: qs.current_level_time_ns,
            max_level_buffers: qs.max_level_buffers,
            max_level_bytes: qs.max_level_bytes,
            max_level_time_ns: qs.max_level_time_ns,
            fill_pct,
        }
    });

    let properties = ie.properties.as_ref().map(|props| {
        props
            .iter()
            .map(|(name, value)| mcm_api::v1::stats::ElementProperty {
                name: name.clone(),
                value: value.clone(),
            })
            .collect()
    });

    let cpu_stats = element_cpu_history
        .get(&*ie.element_name)
        .filter(|h| !h.is_empty())
        .map(|h| Distribution::from_slice(h));

    ElementSnapshot {
        name: ie.element_name.to_string(),
        element_type: ie.element_type.to_string(),
        is_bin: false,
        children: Vec::new(),
        thread_id: ie.thread_id,
        state: ie.state.clone(),
        properties,
        stats: ElementStats {
            processing_time_us: ie.processing_time_us,
            processing_time_stats: ie.processing_time_stats.clone(),
            is_cross_thread: ie.is_cross_thread,
            cpu_pct: ie.estimated_cpu_pct,
            cpu_stats,
            health,
            stutter_events: stutter,
            freeze_events: freeze,
            max_freeze_ms: max_freeze,
            stutter_ratio: ratio,
            queue_stats,
        },
        connections,
        pads,
    }
}

/// Convert an `InternalPadSnapshot` to the public `PadSnapshot` API type.
///
/// Takes a borrow to avoid cloning the pad's raw records (~21KB for 900 records).
fn convert_pad(
    ip: &InternalPadSnapshot,
    direction: PadDirection,
    element_name: &str,
    topo_edges: &[TopologyEdge],
    expected_interval_ms: f64,
    buffer_limit: usize,
) -> PadSnapshot {
    let connections: Vec<PadConnection> = topo_edges
        .iter()
        .filter_map(|te| match direction {
            PadDirection::Src if te.from_node == element_name && *te.from_pad == *ip.pad_name => {
                Some(PadConnection {
                    peer_element: te.to_node.clone(),
                    peer_pad: te.to_pad.clone(),
                    media_type: te.media_type.clone(),
                })
            }
            PadDirection::Sink if te.to_node == element_name && *te.to_pad == *ip.pad_name => {
                Some(PadConnection {
                    peer_element: te.from_node.clone(),
                    peer_pad: te.from_pad.clone(),
                    media_type: te.media_type.clone(),
                })
            }
            _ => None,
        })
        .collect();

    let buffer = if buffer_limit > 0 {
        let records = ip
            .records
            .as_ref()
            .map(|v| v.as_slice())
            .unwrap_or_default();
        let skip = records.len().saturating_sub(buffer_limit);
        records[skip..].to_vec()
    } else {
        Vec::new()
    };

    let bitrate_bps = compute_bitrate(&ip.accumulators, &ip.distribution, expected_interval_ms);

    let avg_gop_size = if ip.total_keyframes > 0 {
        Some(ip.total_buffers as f64 / ip.total_keyframes as f64)
    } else {
        None
    };
    let keyframe_interval_ms = compute_keyframe_interval_distribution(&buffer);

    PadSnapshot {
        name: ip.pad_name.to_string(),
        direction,
        caps: ip.caps.clone(),
        stats: PadStats {
            level: ip.level,
            total_buffers: ip.total_buffers,
            total_keyframes: ip.total_keyframes,
            total_delta_frames: ip.total_delta_frames,
            total_dropped: 0,
            drop_ratio: 0.0,
            bitrate_bps,
            avg_gop_size,
            keyframe_interval_ms,
            last_wall_ns: ip.last_wall_ns,
            accumulators: ip.accumulators.clone(),
            distribution: ip.distribution.clone(),
        },
        buffer,
        connections,
    }
}

/// Estimate bitrate in bits-per-second from pad accumulators or distribution data.
fn compute_bitrate(
    accumulators: &Option<mcm_api::v1::stats::AccumulatorSnapshot>,
    distribution: &Option<mcm_api::v1::stats::DistributionSnapshot>,
    expected_interval_ms: f64,
) -> Option<f64> {
    // From distribution: mean_size_bytes * fps → bytes/sec → bits/sec
    if let Some(ref dist) = distribution {
        if dist.interval.mean > 0.0 {
            let fps = 1000.0 / dist.interval.mean;
            let bps = dist.size.mean * fps * 8.0;
            return Some(bps);
        }
    }
    // From accumulators: mean_size_bytes * fps
    if let Some(ref acc) = accumulators {
        if acc.mean_interval_ms > 0.0 {
            let fps = 1000.0 / acc.mean_interval_ms;
            let bps = acc.mean_size_bytes * fps * 8.0;
            return Some(bps);
        }
    }
    // Fallback: if we know expected interval
    if expected_interval_ms > 0.0 {
        return None;
    }
    None
}

/// Compute keyframe interval distribution from Full-mode raw records.
fn compute_keyframe_interval_distribution(records: &[RawRecord]) -> Option<Distribution> {
    if records.len() < 2 {
        return None;
    }
    let mut intervals_ms = Vec::new();
    let mut last_kf_wall_ns: Option<u64> = None;
    for rec in records {
        if rec.is_keyframe {
            if let Some(prev) = last_kf_wall_ns {
                if rec.wall_ns > prev {
                    intervals_ms.push((rec.wall_ns - prev) as f64 / 1_000_000.0);
                }
            }
            last_kf_wall_ns = Some(rec.wall_ns);
        }
    }
    if intervals_ms.len() >= 2 {
        Some(Distribution::compute(&mut intervals_ms))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcm_api::v1::stats::{HealthStatus, IssueKind, RawRecord};

    fn full_pad(records: Vec<RawRecord>) -> InternalPadSnapshot {
        InternalPadSnapshot {
            pad_name: Arc::from(""),
            level: StatsLevel::Full,
            total_buffers: records.len() as u64,
            total_keyframes: records.iter().filter(|r| r.is_keyframe).count() as u64,
            total_delta_frames: records.iter().filter(|r| !r.is_keyframe).count() as u64,
            last_wall_ns: records.last().map(|r| r.wall_ns).unwrap_or(0),
            accumulators: None,
            distribution: None,
            records: Some(Arc::new(records)),
            caps: None,
        }
    }

    fn minimal_pipeline_snapshot(cpu_pct: f64, freshness_delay: f64) -> PipelineSnapshot {
        PipelineSnapshot {
            name: "p0".to_string(),
            stats: PipelineStats {
                level: StatsLevel::Full,
                window_size: 10,
                expected_interval_ms: 33.3,
                uptime_secs: 1.0,
                health: HealthStatus::Good,
                dominant_issue: IssueKind::Unknown,
                cpu_pct: Some(cpu_pct),
                cpu_stats: None,
                summary: PipelineSummary {
                    total_frames: 1,
                    throughput_fps: 30.0,
                    total_pipeline_freshness_delay_ms: freshness_delay,
                    total_pipeline_causal_latency_ms: None,
                    causal_latency_health: Some(CausalConfidence::High),
                    verdict: "ok".to_string(),
                },
                system: SystemSnapshot::default(),
                restarts: RestartSnapshot::default(),
                root_cause_candidates: Vec::new(),
                thread_bottlenecks: Vec::new(),
            },
            connections: Vec::new(),
            elements: Vec::new(),
            threads: Vec::new(),
        }
    }

    fn element_with_pads(
        src: Option<InternalPadSnapshot>,
        sink: Option<InternalPadSnapshot>,
    ) -> InternalElementSnapshot {
        let mut src_pads = Vec::new();
        let mut sink_pads = Vec::new();
        if let Some(mut s) = src {
            s.pad_name = "src".into();
            src_pads.push(s);
        }
        if let Some(mut s) = sink {
            s.pad_name = "sink".into();
            sink_pads.push(s);
        }
        InternalElementSnapshot {
            element_name: "el".into(),
            element_type: "identity".into(),
            sink_pads,
            src_pads,
            thread_id: None,
            parent_bin: None,
            processing_time_us: None,
            processing_time_stats: None,
            estimated_cpu_pct: None,
            is_cross_thread: None,
            state: None,
            properties: None,
            queue_stats: None,
        }
    }

    #[test]
    fn causal_latency_handles_duplicate_pts_without_overwrite_bias() {
        let src_records = vec![
            RawRecord {
                wall_ns: 1_000_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 1_100_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: false,
            },
        ];
        let sink_records = vec![
            RawRecord {
                wall_ns: 1_050_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 1_250_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: false,
            },
        ];
        let from = element_with_pads(Some(full_pad(src_records)), None);
        let to = element_with_pads(None, Some(full_pad(sink_records)));

        let (dist, rate, matched, confidence) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        let dist = dist.expect("expected causal latency distribution");

        assert_eq!(dist.count, 2);
        assert!((dist.mean - 100.0).abs() < 1e-6);
        assert_eq!(rate, Some(1.0));
        assert_eq!(matched, Some(2));
        assert_eq!(confidence, Some(CausalConfidence::Low));
    }

    #[test]
    fn aggregate_causal_latency_health_is_weighted_by_samples() {
        let edges = vec![
            InternalEdgeDelay {
                edge_idx: 0,
                freshness_delay_ms: 1.0,
                causal_latency_ms: None,
                causal_match_rate: Some(0.9),
                causal_matched_samples: Some(100),
                causal_confidence: Some(CausalConfidence::High),
            },
            InternalEdgeDelay {
                edge_idx: 1,
                freshness_delay_ms: 1.0,
                causal_latency_ms: None,
                causal_match_rate: Some(0.1),
                causal_matched_samples: Some(5),
                causal_confidence: Some(CausalConfidence::Low),
            },
        ];

        assert_eq!(
            aggregate_causal_latency_health(&edges),
            Some(CausalConfidence::High)
        );
    }

    #[test]
    fn window_size_bounds_are_enforced() {
        assert!(is_valid_window_size(1));
        assert!(is_valid_window_size(MAX_WINDOW_SIZE));
        assert!(!is_valid_window_size(0));
        assert!(!is_valid_window_size(MAX_WINDOW_SIZE + 1));
    }

    #[test]
    fn causal_confidence_thresholds() {
        assert_eq!(causal_confidence_label(0.8, 50), CausalConfidence::High);
        assert_eq!(causal_confidence_label(0.4, 20), CausalConfidence::Medium);
        assert_eq!(causal_confidence_label(0.39, 20), CausalConfidence::Low);
    }

    #[test]
    fn stutter_and_freeze_are_detected_from_records() {
        let records = vec![
            RawRecord {
                wall_ns: 0,
                pts_ns: Some(1),
                size: 100,
                is_keyframe: true,
            },
            RawRecord {
                wall_ns: 40_000_000,
                pts_ns: Some(2),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 600_000_000,
                pts_ns: Some(3),
                size: 100,
                is_keyframe: false,
            },
        ];
        let pad = full_pad(records);
        let (stutter, freeze, max_freeze_ms, ratio) =
            diagnostics::stutter_freeze_from_pad(&pad, 33.3, 3);
        assert!(stutter >= 1);
        assert!(freeze >= 1);
        assert!(max_freeze_ms >= 500.0);
        assert!(ratio > 0.0);
    }

    #[test]
    fn health_status_marks_cpu_saturation_as_bad() {
        let snapshot = minimal_pipeline_snapshot(98.0, 2.0);
        let (status, issue) = health::pipeline_health(&snapshot);
        assert_eq!(status, HealthStatus::Bad);
        assert_eq!(issue, IssueKind::CpuSaturation);
    }

    #[test]
    fn compute_summary_uses_unique_pts_for_packetized_streams() {
        let start = 1_000_000_000_u64;
        let mut records = Vec::new();
        for frame_idx in 0..30_u64 {
            let frame_wall = start + frame_idx * 33_333_333;
            for pkt_idx in 0..4_u64 {
                records.push(RawRecord {
                    wall_ns: frame_wall + pkt_idx * 1_000,
                    pts_ns: Some(frame_idx),
                    size: 1400,
                    is_keyframe: frame_idx == 0,
                });
            }
        }

        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        elements.insert(
            "rtspsrc0".to_string(),
            element_with_pads(Some(full_pad(records)), None),
        );

        let empty_edges: Vec<InternalEdgeDelay> = Vec::new();
        let summary = compute_summary(&elements, &empty_edges, None, &mut Vec::new());
        assert_eq!(summary.total_frames, 30);
        assert!(summary.throughput_fps > 20.0);
        assert!(summary.throughput_fps < 60.0);
    }

    // ── CPU attribution tests ──

    /// Helper: build a lite InternalPadSnapshot with a specific `last_wall_ns`.
    fn lite_pad_at(wall_ns: u64) -> InternalPadSnapshot {
        InternalPadSnapshot {
            pad_name: Arc::from(""),
            level: StatsLevel::Lite,
            total_buffers: 1,
            total_keyframes: 0,
            total_delta_frames: 1,
            last_wall_ns: wall_ns,
            accumulators: None,
            distribution: None,
            records: None,
            caps: None,
        }
    }

    /// Helper: build an InternalElementSnapshot for a filter element (has both sink and src pads)
    /// on a given thread, with specified wall-clock timestamps and pre-computed processing time.
    fn filter_element(
        name: &str,
        element_type: &str,
        tid: u32,
        sink_wall_ns: u64,
        src_wall_ns: u64,
    ) -> (String, InternalElementSnapshot) {
        let mut sink_pad = lite_pad_at(sink_wall_ns);
        sink_pad.pad_name = "sink".into();
        let mut src_pad = lite_pad_at(src_wall_ns);
        src_pad.pad_name = "src".into();
        let processing_time_us = if src_wall_ns > sink_wall_ns {
            Some((src_wall_ns - sink_wall_ns) as f64 / 1_000.0)
        } else {
            None
        };
        (
            name.to_string(),
            InternalElementSnapshot {
                element_name: Arc::from(name),
                element_type: Arc::from(element_type),
                sink_pads: vec![sink_pad],
                src_pads: vec![src_pad],
                thread_id: Some(tid),
                parent_bin: None,
                processing_time_us,
                processing_time_stats: None,
                estimated_cpu_pct: None,
                is_cross_thread: None,
                state: None,
                properties: None,
                queue_stats: None,
            },
        )
    }

    /// Helper: build an InternalElementSnapshot for a source element (src pad only).
    fn source_element(
        name: &str,
        element_type: &str,
        tid: u32,
    ) -> (String, InternalElementSnapshot) {
        let mut src_pad = lite_pad_at(1_000_000_000);
        src_pad.pad_name = "src".into();
        (
            name.to_string(),
            InternalElementSnapshot {
                element_name: Arc::from(name),
                element_type: Arc::from(element_type),
                sink_pads: Vec::new(),
                src_pads: vec![src_pad],
                thread_id: Some(tid),
                parent_bin: None,
                processing_time_us: None,
                processing_time_stats: None,
                estimated_cpu_pct: None,
                is_cross_thread: None,
                state: None,
                properties: None,
                queue_stats: None,
            },
        )
    }

    /// Helper: build an InternalElementSnapshot for a sink element (sink pad only).
    fn sink_element(name: &str, element_type: &str, tid: u32) -> (String, InternalElementSnapshot) {
        let mut sink_pad = lite_pad_at(1_000_000_000);
        sink_pad.pad_name = "sink".into();
        (
            name.to_string(),
            InternalElementSnapshot {
                element_name: Arc::from(name),
                element_type: Arc::from(element_type),
                sink_pads: vec![sink_pad],
                src_pads: Vec::new(),
                thread_id: Some(tid),
                parent_bin: None,
                processing_time_us: None,
                processing_time_stats: None,
                estimated_cpu_pct: None,
                is_cross_thread: None,
                state: None,
                properties: None,
                queue_stats: None,
            },
        )
    }

    /// Helper: build an InternalElementSnapshot for a tee with skipped src.
    fn tee_element(name: &str, tid: u32) -> (String, InternalElementSnapshot) {
        let mut sink_pad = lite_pad_at(1_000_000_000);
        sink_pad.pad_name = "sink".into();
        (
            name.to_string(),
            InternalElementSnapshot {
                element_name: Arc::from(name),
                element_type: "tee".into(),
                sink_pads: vec![sink_pad],
                src_pads: Vec::new(),
                thread_id: Some(tid),
                parent_bin: None,
                processing_time_us: None,
                processing_time_stats: None,
                estimated_cpu_pct: None,
                is_cross_thread: None,
                state: None,
                properties: None,
                queue_stats: None,
            },
        )
    }

    fn make_tracker(entries: &[(u32, &str, f64)]) -> ThreadCpuTracker {
        let mut tracker = ThreadCpuTracker::new();
        for &(tid, name, cpu) in entries {
            tracker.insert_for_test(tid, name, cpu);
        }
        tracker
    }

    #[test]
    fn cpu_attribution_all_filter_elements() {
        // Three filter elements on one thread, each with different processing times.
        // sink_wall=1000ns, src_wall varies to give processing times of 100, 200, 300 us.
        let base = 1_000_000_000_u64;
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = filter_element("h264parse0", "h264parse", 100, base, base + 100_000);
        elements.insert(n, e);
        let (n, e) = filter_element("capsfilter0", "capsfilter", 100, base, base + 200_000);
        elements.insert(n, e);
        let (n, e) = filter_element("rtph264pay0", "rtph264pay", 100, base, base + 300_000);
        elements.insert(n, e);

        let tracker = make_tracker(&[(100, "src-thread", 12.0)]);
        let (total_cpu, groups) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 12.0).abs() < 1e-9);
        assert_eq!(groups.len(), 1);

        // Processing times: 100, 200, 300 us => fractions 1/6, 2/6, 3/6 of 12%
        let h264 = elements["h264parse0"].estimated_cpu_pct.unwrap();
        let caps = elements["capsfilter0"].estimated_cpu_pct.unwrap();
        let rtp = elements["rtph264pay0"].estimated_cpu_pct.unwrap();
        assert!((h264 - 2.0).abs() < 1e-9, "h264parse: {h264}");
        assert!((caps - 4.0).abs() < 1e-9, "capsfilter: {caps}");
        assert!((rtp - 6.0).abs() < 1e-9, "rtph264pay: {rtp}");

        // Sum equals thread CPU.
        assert!((h264 + caps + rtp - 12.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_attribution_source_plus_filters() {
        // Source element + 2 filters on the same thread. Source has no sink pad,
        // so it should get the residual CPU.
        let base = 1_000_000_000_u64;
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = source_element("videotestsrc0", "videotestsrc", 200);
        elements.insert(n, e);
        // Two filters with equal processing time (100us each).
        let (n, e) = filter_element("h264parse0", "h264parse", 200, base, base + 100_000);
        elements.insert(n, e);
        let (n, e) = filter_element("capsfilter0", "capsfilter", 200, base, base + 100_000);
        elements.insert(n, e);

        let tracker = make_tracker(&[(200, "vtsrc-thread", 10.0)]);
        let (total_cpu, _) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 10.0).abs() < 1e-9);

        // Filters share equally: 5.0% each (100us / 200us * 10%).
        let h264 = elements["h264parse0"].estimated_cpu_pct.unwrap();
        let caps = elements["capsfilter0"].estimated_cpu_pct.unwrap();
        assert!((h264 - 5.0).abs() < 1e-9, "h264parse: {h264}");
        assert!((caps - 5.0).abs() < 1e-9, "capsfilter: {caps}");

        // Source gets the residual: 10% - 5% - 5% = 0%.
        let src = elements["videotestsrc0"].estimated_cpu_pct.unwrap();
        assert!((src - 0.0).abs() < 1e-9, "videotestsrc residual: {src}");

        // Sum invariant.
        assert!((h264 + caps + src - total_cpu).abs() < 1e-9);
    }

    #[test]
    fn cpu_attribution_single_unmeasured_element_gets_full_thread_cpu() {
        // A source element is the only element on a thread.
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = source_element("v4l2src0", "v4l2src", 300);
        elements.insert(n, e);

        let tracker = make_tracker(&[(300, "v4l2-thread", 8.5)]);
        let (total_cpu, groups) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 8.5).abs() < 1e-9);
        assert_eq!(groups.len(), 1);

        // Entire thread CPU goes to the single unmeasured element.
        let src_cpu = elements["v4l2src0"].estimated_cpu_pct.unwrap();
        assert!((src_cpu - 8.5).abs() < 1e-9, "v4l2src: {src_cpu}");
    }

    #[test]
    fn cpu_attribution_multiple_unmeasured_elements_share_equally() {
        // Two tee elements on the same thread, no filter elements.
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = tee_element("videoTee", 400);
        elements.insert(n, e);
        let (n, e) = tee_element("rtpTee", 400);
        elements.insert(n, e);

        let tracker = make_tracker(&[(400, "tee-thread", 6.0)]);
        let (total_cpu, _) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 6.0).abs() < 1e-9);

        let tee1 = elements["videoTee"].estimated_cpu_pct.unwrap();
        let tee2 = elements["rtpTee"].estimated_cpu_pct.unwrap();
        assert!((tee1 - 3.0).abs() < 1e-9, "videoTee: {tee1}");
        assert!((tee2 - 3.0).abs() < 1e-9, "rtpTee: {tee2}");
        assert!((tee1 + tee2 - total_cpu).abs() < 1e-9);
    }

    #[test]
    fn cpu_attribution_zero_thread_cpu_no_nan() {
        // Thread has 0% CPU -- all elements should get 0 or None, no NaN.
        let base = 1_000_000_000_u64;
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = source_element("src0", "videotestsrc", 500);
        elements.insert(n, e);
        let (n, e) = filter_element("parse0", "h264parse", 500, base, base + 100_000);
        elements.insert(n, e);

        let tracker = make_tracker(&[(500, "idle-thread", 0.0)]);
        let (total_cpu, _) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 0.0).abs() < 1e-9);

        // Filter gets processing_time_us but no CPU (thread at 0%).
        assert!(elements["parse0"].processing_time_us.is_some());
        assert!(elements["parse0"].estimated_cpu_pct.is_none());

        // Source has no sink pad, no processing_time. With 0% thread CPU,
        // the residual branch is skipped.
        assert!(elements["src0"].estimated_cpu_pct.is_none());
    }

    #[test]
    fn cpu_attribution_sum_invariant_mixed_threads() {
        // Two threads: thread 100 has source+filters, thread 200 has sink.
        let base = 1_000_000_000_u64;
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = source_element("src0", "videotestsrc", 100);
        elements.insert(n, e);
        let (n, e) = filter_element("parse0", "h264parse", 100, base, base + 50_000);
        elements.insert(n, e);
        let (n, e) = filter_element("pay0", "rtph264pay", 100, base, base + 150_000);
        elements.insert(n, e);
        let (n, e) = sink_element("sink0", "multiudpsink", 200);
        elements.insert(n, e);

        let tracker = make_tracker(&[(100, "src-thread", 20.0), (200, "sink-thread", 5.0)]);
        let (total_cpu, groups) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 25.0).abs() < 1e-9);
        assert_eq!(groups.len(), 2);

        // Thread 100: parse gets 50/200 * 20 = 5%, pay gets 150/200 * 20 = 15%,
        // source gets residual = 20 - 5 - 15 = 0%.
        let src = elements["src0"].estimated_cpu_pct.unwrap();
        let parse = elements["parse0"].estimated_cpu_pct.unwrap();
        let pay = elements["pay0"].estimated_cpu_pct.unwrap();
        assert!((parse - 5.0).abs() < 1e-9, "parse: {parse}");
        assert!((pay - 15.0).abs() < 1e-9, "pay: {pay}");
        assert!((src - 0.0).abs() < 1e-9, "src residual: {src}");
        assert!((src + parse + pay - 20.0).abs() < 1e-9);

        // Thread 200: sink gets full residual = 5%.
        let sink = elements["sink0"].estimated_cpu_pct.unwrap();
        assert!((sink - 5.0).abs() < 1e-9, "sink: {sink}");
    }

    #[test]
    fn cpu_attribution_no_thread_id_elements_are_ignored() {
        // Element without a thread_id should be ignored entirely.
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        elements.insert(
            "orphan".to_string(),
            InternalElementSnapshot {
                element_name: "orphan".into(),
                element_type: "identity".into(),
                sink_pads: Vec::new(),
                src_pads: Vec::new(),
                thread_id: None,
                parent_bin: None,
                processing_time_us: None,
                processing_time_stats: None,
                estimated_cpu_pct: None,
                is_cross_thread: None,
                state: None,
                properties: None,
                queue_stats: None,
            },
        );

        let tracker = make_tracker(&[]);
        let (total_cpu, groups) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 0.0).abs() < 1e-9);
        assert!(groups.is_empty());
        assert!(elements["orphan"].estimated_cpu_pct.is_none());
    }

    #[test]
    fn cpu_attribution_empty_elements() {
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let tracker = make_tracker(&[(100, "thread", 5.0)]);
        let (total_cpu, groups) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 0.0).abs() < 1e-9);
        assert!(groups.is_empty());
    }

    #[test]
    fn cpu_attribution_zero_processing_time_treated_as_unmeasured() {
        // Element where src and sink have the same wall timestamp (0 processing
        // time) should be treated as unmeasured and receive residual CPU.
        let base = 1_000_000_000_u64;
        let mut elements: FxHashMap<String, InternalElementSnapshot> = FxHashMap::default();
        let (n, e) = filter_element("identity0", "identity", 600, base, base);
        elements.insert(n, e);
        let (n, e) = source_element("src0", "videotestsrc", 600);
        elements.insert(n, e);

        let tracker = make_tracker(&[(600, "thread", 4.0)]);
        let (total_cpu, _) = attribute_cpu(&mut elements, &tracker);

        assert!((total_cpu - 4.0).abs() < 1e-9);

        // identity has equal src/sink timestamps, so processing_time is not
        // computed (strict inequality). Both elements are unmeasured and share
        // the thread CPU equally.
        assert!(elements["identity0"].processing_time_us.is_none());
        let id_cpu = elements["identity0"].estimated_cpu_pct.unwrap();
        let src_cpu = elements["src0"].estimated_cpu_pct.unwrap();
        assert!((id_cpu - 2.0).abs() < 1e-9, "identity: {id_cpu}");
        assert!((src_cpu - 2.0).abs() < 1e-9, "source: {src_cpu}");
        assert!((id_cpu + src_cpu - total_cpu).abs() < 1e-9);
    }

    // ── compute_causal_edge_latency tests ──

    #[test]
    fn causal_latency_empty_records_returns_zero() {
        let from = element_with_pads(Some(full_pad(vec![])), None);
        let to = element_with_pads(None, Some(full_pad(vec![])));
        let (dist, rate, matched, confidence) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        assert!(dist.is_none());
        assert_eq!(rate, Some(0.0));
        assert_eq!(matched, Some(0));
        assert_eq!(confidence, Some(CausalConfidence::Low));
    }

    #[test]
    fn causal_latency_no_pts_returns_zero_rate() {
        let src = vec![RawRecord {
            wall_ns: 1_000_000_000,
            pts_ns: None,
            size: 100,
            is_keyframe: false,
        }];
        let sink = vec![RawRecord {
            wall_ns: 1_050_000_000,
            pts_ns: None,
            size: 100,
            is_keyframe: false,
        }];
        let from = element_with_pads(Some(full_pad(src)), None);
        let to = element_with_pads(None, Some(full_pad(sink)));
        let (dist, rate, matched, confidence) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        assert!(dist.is_none());
        assert_eq!(rate, Some(0.0));
        assert_eq!(matched, Some(0));
        assert_eq!(confidence, Some(CausalConfidence::Low));
    }

    #[test]
    fn causal_latency_single_matched_pair() {
        let src = vec![RawRecord {
            wall_ns: 1_000_000_000,
            pts_ns: Some(100),
            size: 100,
            is_keyframe: false,
        }];
        let sink = vec![RawRecord {
            wall_ns: 1_005_000_000,
            pts_ns: Some(100),
            size: 100,
            is_keyframe: false,
        }];
        let from = element_with_pads(Some(full_pad(src)), None);
        let to = element_with_pads(None, Some(full_pad(sink)));
        let (dist, rate, matched, _) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        let dist = dist.expect("expected distribution");
        assert_eq!(dist.count, 1);
        assert!(
            (dist.mean - 5.0).abs() < 1e-6,
            "expected 5ms, got {}",
            dist.mean
        );
        assert_eq!(rate, Some(1.0));
        assert_eq!(matched, Some(1));
    }

    #[test]
    fn causal_latency_multiple_distinct_pts() {
        let src = vec![
            RawRecord {
                wall_ns: 1_000_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: true,
            },
            RawRecord {
                wall_ns: 1_033_000_000,
                pts_ns: Some(200),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 1_066_000_000,
                pts_ns: Some(300),
                size: 100,
                is_keyframe: false,
            },
        ];
        let sink = vec![
            RawRecord {
                wall_ns: 1_002_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: true,
            },
            RawRecord {
                wall_ns: 1_035_000_000,
                pts_ns: Some(200),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 1_068_000_000,
                pts_ns: Some(300),
                size: 100,
                is_keyframe: false,
            },
        ];
        let from = element_with_pads(Some(full_pad(src)), None);
        let to = element_with_pads(None, Some(full_pad(sink)));
        let (dist, rate, matched, _) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        let dist = dist.expect("expected distribution");
        assert_eq!(dist.count, 3);
        assert_eq!(matched, Some(3));
        assert_eq!(rate, Some(1.0));
        assert!(
            (dist.mean - 2.0).abs() < 0.1,
            "expected ~2ms, got {}",
            dist.mean
        );
    }

    #[test]
    fn causal_latency_unmatched_pts_reduces_rate() {
        let src = vec![
            RawRecord {
                wall_ns: 1_000_000_000,
                pts_ns: Some(100),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 1_033_000_000,
                pts_ns: Some(200),
                size: 100,
                is_keyframe: false,
            },
            RawRecord {
                wall_ns: 1_066_000_000,
                pts_ns: Some(300),
                size: 100,
                is_keyframe: false,
            },
        ];
        // Sink only has PTS=100, missing 200 and 300
        let sink = vec![RawRecord {
            wall_ns: 1_005_000_000,
            pts_ns: Some(100),
            size: 100,
            is_keyframe: false,
        }];
        let from = element_with_pads(Some(full_pad(src)), None);
        let to = element_with_pads(None, Some(full_pad(sink)));
        let (dist, rate, matched, _) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        let dist = dist.expect("expected distribution");
        assert_eq!(dist.count, 1);
        assert_eq!(matched, Some(1));
        let r = rate.unwrap();
        assert!((r - 1.0 / 3.0).abs() < 1e-6, "expected 1/3, got {r}");
    }

    #[test]
    fn causal_latency_large_record_set_correctness() {
        // Simulate 900 records at 30fps with 2ms transit latency
        let n = 900;
        let interval_ns: u64 = 33_333_333;
        let latency_ns: u64 = 2_000_000;
        let src: Vec<RawRecord> = (0..n)
            .map(|i| RawRecord {
                wall_ns: 1_000_000_000 + i as u64 * interval_ns,
                pts_ns: Some(i as u64 * interval_ns),
                size: 5000,
                is_keyframe: i % 30 == 0,
            })
            .collect();
        let sink: Vec<RawRecord> = (0..n)
            .map(|i| RawRecord {
                wall_ns: 1_000_000_000 + i as u64 * interval_ns + latency_ns,
                pts_ns: Some(i as u64 * interval_ns),
                size: 5000,
                is_keyframe: i % 30 == 0,
            })
            .collect();
        let from = element_with_pads(Some(full_pad(src)), None);
        let to = element_with_pads(None, Some(full_pad(sink)));
        let (dist, rate, matched, confidence) =
            compute_causal_edge_latency(&from, &to, "src", "sink", &mut MergeJoinScratch::new());
        let dist = dist.expect("expected distribution");
        assert_eq!(dist.count, n as u64);
        assert_eq!(matched, Some(n as u64));
        assert_eq!(rate, Some(1.0));
        assert!(
            (dist.mean - 2.0).abs() < 0.01,
            "expected 2.0ms, got {}",
            dist.mean
        );
        assert!(dist.std < 0.01, "expected near-zero std, got {}", dist.std);
        assert_eq!(confidence, Some(CausalConfidence::High));
    }

    // ── compute_intra_element_processing_time tests ──

    fn full_element_with_sink_src(
        sink_records: Vec<RawRecord>,
        src_records: Vec<RawRecord>,
    ) -> (String, InternalElementSnapshot) {
        let mut sink_pad = InternalPadSnapshot {
            pad_name: "sink".into(),
            level: StatsLevel::Full,
            total_buffers: sink_records.len() as u64,
            total_keyframes: 0,
            total_delta_frames: 0,
            last_wall_ns: sink_records.last().map(|r| r.wall_ns).unwrap_or(0),
            accumulators: None,
            distribution: None,
            records: Some(Arc::new(sink_records)),
            caps: None,
        };
        let mut src_pad = InternalPadSnapshot {
            pad_name: "src".into(),
            level: StatsLevel::Full,
            total_buffers: src_records.len() as u64,
            total_keyframes: 0,
            total_delta_frames: 0,
            last_wall_ns: src_records.last().map(|r| r.wall_ns).unwrap_or(0),
            accumulators: None,
            distribution: None,
            records: Some(Arc::new(src_records)),
            caps: None,
        };
        (
            "el0".to_string(),
            InternalElementSnapshot {
                element_name: "el0".into(),
                element_type: "identity".into(),
                sink_pads: vec![sink_pad],
                src_pads: vec![src_pad],
                thread_id: Some(100),
                parent_bin: None,
                processing_time_us: Some(50.0), // atomic fallback, should be overwritten
                processing_time_stats: None,
                estimated_cpu_pct: None,
                is_cross_thread: Some(false),
                state: None,
                properties: None,
                queue_stats: None,
            },
        )
    }

    #[test]
    fn intra_element_pts_matched_basic() {
        let base = 1_000_000_000_u64;
        let pts_interval = 33_333_333; // ~30fps

        let sink_records: Vec<RawRecord> = (0..10)
            .map(|i| RawRecord {
                wall_ns: base + i * pts_interval,
                pts_ns: Some(i * pts_interval),
                size: 1000,
                is_keyframe: false,
            })
            .collect();
        // Src records arrive 500µs (500_000 ns) after sink for each PTS.
        let src_records: Vec<RawRecord> = (0..10)
            .map(|i| RawRecord {
                wall_ns: base + i * pts_interval + 500_000,
                pts_ns: Some(i * pts_interval),
                size: 1000,
                is_keyframe: false,
            })
            .collect();

        let (name, snap) = full_element_with_sink_src(sink_records, src_records);
        let mut elements = FxHashMap::default();
        elements.insert(name, snap);

        compute_intra_element_processing_time(&mut elements, &mut MergeJoinScratch::new());

        let el = &elements["el0"];
        let dist = el
            .processing_time_stats
            .as_ref()
            .expect("expected distribution");
        assert_eq!(dist.count, 10);
        assert!(
            (dist.mean - 500.0).abs() < 1.0,
            "expected ~500 µs, got {} µs",
            dist.mean
        );
        assert!(dist.std < 1.0, "expected near-zero std, got {}", dist.std);
        assert!(
            (el.processing_time_us.unwrap() - 500.0).abs() < 1.0,
            "mean should replace atomic fallback"
        );
    }

    #[test]
    fn intra_element_pts_matched_cross_thread_queue() {
        let base = 1_000_000_000_u64;
        let pts_interval = 33_333_333;

        let sink_records: Vec<RawRecord> = (0..10)
            .map(|i| RawRecord {
                wall_ns: base + i * pts_interval,
                pts_ns: Some(i * pts_interval),
                size: 1000,
                is_keyframe: false,
            })
            .collect();
        // Src departs 5ms later (simulating queue buffering).
        let src_records: Vec<RawRecord> = (0..10)
            .map(|i| RawRecord {
                wall_ns: base + i * pts_interval + 5_000_000,
                pts_ns: Some(i * pts_interval),
                size: 1000,
                is_keyframe: false,
            })
            .collect();

        let (name, mut snap) = full_element_with_sink_src(sink_records, src_records);
        snap.is_cross_thread = Some(true);
        snap.element_type = "queue".into();
        let mut elements = FxHashMap::default();
        elements.insert(name, snap);

        compute_intra_element_processing_time(&mut elements, &mut MergeJoinScratch::new());

        let el = &elements["el0"];
        let dist = el
            .processing_time_stats
            .as_ref()
            .expect("expected distribution");
        assert_eq!(dist.count, 10);
        // 5ms = 5000 µs
        assert!(
            (dist.mean - 5000.0).abs() < 1.0,
            "expected ~5000 µs queuing delay, got {} µs",
            dist.mean
        );
    }

    #[test]
    fn intra_element_pts_matched_no_records_no_crash() {
        let (name, snap) = full_element_with_sink_src(vec![], vec![]);
        let mut elements = FxHashMap::default();
        elements.insert(name, snap);

        compute_intra_element_processing_time(&mut elements, &mut MergeJoinScratch::new());

        let el = &elements["el0"];
        // No records → no PTS match, atomic fallback preserved.
        assert!(el.processing_time_stats.is_none());
        assert!(
            (el.processing_time_us.unwrap() - 50.0).abs() < 1e-9,
            "atomic fallback should be preserved"
        );
    }

    #[test]
    fn intra_element_pts_matched_variable_processing_time() {
        let base = 1_000_000_000_u64;
        let pts_interval = 33_333_333;

        let sink_records: Vec<RawRecord> = (0..100)
            .map(|i| RawRecord {
                wall_ns: base + i * pts_interval,
                pts_ns: Some(i * pts_interval),
                size: 1000,
                is_keyframe: false,
            })
            .collect();
        // Processing time alternates between 100µs and 900µs → mean ~500µs.
        let src_records: Vec<RawRecord> = (0..100)
            .map(|i| {
                let delay = if i % 2 == 0 { 100_000 } else { 900_000 };
                RawRecord {
                    wall_ns: base + i * pts_interval + delay,
                    pts_ns: Some(i * pts_interval),
                    size: 1000,
                    is_keyframe: false,
                }
            })
            .collect();

        let (name, snap) = full_element_with_sink_src(sink_records, src_records);
        let mut elements = FxHashMap::default();
        elements.insert(name, snap);

        compute_intra_element_processing_time(&mut elements, &mut MergeJoinScratch::new());

        let el = &elements["el0"];
        let dist = el
            .processing_time_stats
            .as_ref()
            .expect("expected distribution");
        assert_eq!(dist.count, 100);
        assert!(
            (dist.mean - 500.0).abs() < 1.0,
            "expected ~500 µs mean, got {} µs",
            dist.mean
        );
        assert!(
            (dist.min - 100.0).abs() < 1.0,
            "expected ~100 µs min, got {} µs",
            dist.min
        );
        assert!(
            (dist.max - 900.0).abs() < 1.0,
            "expected ~900 µs max, got {} µs",
            dist.max
        );
        assert!(
            dist.std > 100.0,
            "expected significant std, got {}",
            dist.std
        );
        // Median of 50×100 + 50×900 is either 100 or 900 (bimodal).
        assert!(
            dist.median == 100.0 || dist.median == 900.0,
            "expected 100 or 900 µs median, got {} µs",
            dist.median
        );
    }
}
