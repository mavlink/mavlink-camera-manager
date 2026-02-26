//! Lock-free per-element pad probe storage.
//!
//! Two backends selectable at construction time:
//!
//! - **Lite** (`LitePadBuffer`): atomic accumulators — O(1) memory.
//!   Provides mean, variance, min, max, throughput via differentiation between
//!   snapshots. Ideal for production on resource-constrained devices.
//!
//! - **Full** (`FullPadBuffer`): atomic ring buffer — O(window) memory.
//!   Provides everything Lite does plus percentiles, distributions, adaptive
//!   stutter detection, scatter data. For debugging / analysis sessions.
//!
//! Both use lock-free atomics on the write path (GStreamer streaming threads).
//! The Full backend additionally caches distribution results behind a `Mutex`
//! that is only acquired on the read path (API snapshot). The hot write path
//! (`record()`) never contends on the Mutex.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use mcm_api::v1::stats::{
    AccumulatorSnapshot, Distribution, DistributionSnapshot, RawRecord, StatsLevel,
};

/// Internal pad snapshot used during computation. Carries raw records
/// and is organized by direction in ElementProbe. Converted to the
/// public `PadSnapshot` API type at the output boundary.
#[derive(Debug, Clone)]
pub(crate) struct InternalPadSnapshot {
    pub pad_name: Arc<str>,
    pub level: StatsLevel,
    pub total_buffers: u64,
    pub total_keyframes: u64,
    pub total_delta_frames: u64,
    pub last_wall_ns: u64,
    pub accumulators: Option<AccumulatorSnapshot>,
    pub distribution: Option<DistributionSnapshot>,
    /// Shared records buffer. Uses `Arc` so that FullPadBuffer cache hits
    /// return an `Arc::clone()` (~1ns) instead of a full Vec clone (~2μs).
    pub records: Option<Arc<Vec<RawRecord>>>,
    /// Current negotiated caps string, populated at snapshot time.
    pub caps: Option<String>,
}

/// Internal element snapshot with separate sink/src pad lists.
/// Used during computation (edge delays, CPU attribution, diagnostics).
/// Converted to the public `ElementSnapshot` API type at the output boundary.
#[derive(Debug, Clone)]
pub(crate) struct InternalElementSnapshot {
    pub element_name: Arc<str>,
    pub element_type: Arc<str>,
    pub sink_pads: Vec<InternalPadSnapshot>,
    pub src_pads: Vec<InternalPadSnapshot>,
    pub thread_id: Option<u32>,
    /// Immediate parent bin name (`None` if direct child of the pipeline).
    pub parent_bin: Option<Arc<str>>,
    /// Mean per-buffer causal processing time (µs). Computed from atomic
    /// accumulators (Lite) or PTS-matched distribution (Full).
    pub processing_time_us: Option<f64>,
    /// Full distribution of per-buffer processing time (µs), from PTS-matched
    /// intra-element measurement. Only available in Full mode.
    pub processing_time_stats: Option<mcm_api::v1::stats::Distribution>,
    pub estimated_cpu_pct: Option<f64>,
    /// `true` if sink and src pads are driven by different threads (e.g. queue).
    /// When cross-thread, the atomic-based processing time includes queuing delay;
    /// the PTS-matched distribution (if available) is the true transit time.
    pub is_cross_thread: Option<bool>,
    /// Current GStreamer state name (e.g. "playing", "paused").
    pub state: Option<String>,
    /// All GObject properties as name-value string pairs.
    pub properties: Option<Vec<(String, String)>>,
    /// Queue fill-level stats (only for queue/queue2 elements).
    pub queue_stats: Option<InternalQueueStats>,
}

/// Raw queue fill-level data collected at snapshot time.
#[derive(Debug, Clone)]
pub(crate) struct InternalQueueStats {
    pub current_level_buffers: u32,
    pub current_level_bytes: u64,
    pub current_level_time_ns: u64,
    pub max_level_buffers: u32,
    pub max_level_bytes: u64,
    pub max_level_time_ns: u64,
}

// ── PadBuffer enum (dispatches to Lite or Full) ──

/// Per-pad measurement buffer. Wraps either the lightweight accumulator or
/// the full ring buffer depending on the configured stats level.
///
/// Always stored behind `Arc<PadBuffer>` (heap-allocated), so the variant
/// size difference does not cause stack bloat.
#[allow(clippy::large_enum_variant)]
pub enum PadBuffer {
    Lite(LitePadBuffer),
    Full(FullPadBuffer),
}

impl PadBuffer {
    /// Create a new buffer for the given stats level and window capacity.
    pub fn new(level: StatsLevel, capacity: usize) -> Self {
        match level {
            StatsLevel::Lite => Self::Lite(LitePadBuffer::new()),
            StatsLevel::Full => Self::Full(FullPadBuffer::new(capacity)),
        }
    }

    /// Record a buffer observation. Called from the GStreamer streaming thread.
    ///
    /// `wall_ns` must be obtained by the caller (one `clock_gettime` per probe
    /// callback). `size` is the buffer payload size in bytes. `is_keyframe` is
    /// `!DELTA_UNIT`.
    #[inline]
    pub fn record(&self, wall_ns: u64, pts_ns: Option<u64>, size: u32, is_keyframe: bool) {
        match self {
            Self::Lite(b) => b.record(wall_ns, size, is_keyframe),
            Self::Full(b) => b.record(wall_ns, pts_ns, size, is_keyframe),
        }
    }

    /// Last wall-clock timestamp recorded on this pad (lock-free read).
    /// Used by downstream probes to compute inter-element delay without an
    /// extra syscall.
    #[inline]
    pub fn last_wall_ns(&self) -> u64 {
        match self {
            Self::Lite(b) => b.last_wall_ns.load(Ordering::Relaxed),
            Self::Full(b) => b.last_wall_ns.load(Ordering::Relaxed),
        }
    }

    /// Total number of buffers recorded (monotonic counter).
    #[inline]
    pub fn total_buffers(&self) -> u64 {
        match self {
            Self::Lite(b) => b.total_buffers.load(Ordering::Relaxed),
            Self::Full(b) => b.total_buffers.load(Ordering::Relaxed),
        }
    }

    /// Total keyframes recorded (monotonic counter).
    #[inline]
    pub fn total_keyframes(&self) -> u64 {
        match self {
            Self::Lite(b) => b.total_keyframes.load(Ordering::Relaxed),
            Self::Full(b) => b.total_keyframes.load(Ordering::Relaxed),
        }
    }

    /// Total delta (non-keyframe) buffers recorded (monotonic counter).
    #[inline]
    pub fn total_delta_frames(&self) -> u64 {
        match self {
            Self::Lite(b) => b.total_delta_frames.load(Ordering::Relaxed),
            Self::Full(b) => b.total_delta_frames.load(Ordering::Relaxed),
        }
    }

    /// Produce a snapshot of this pad's statistics.
    ///
    /// - **Lite**: returns accumulated counters and running aggregates. The
    ///   caller should differentiate two successive snapshots to get rates.
    /// - **Full**: reads the ring buffer and computes full distributions.
    pub(crate) fn snapshot(&self) -> InternalPadSnapshot {
        match self {
            Self::Lite(b) => b.snapshot(),
            Self::Full(b) => b.snapshot(),
        }
    }

    /// Read raw records without computing distributions (Full mode only).
    /// Returns `None` for Lite mode buffers. This is the fast-path used by the
    /// samples API to avoid the expense of computing 6 sorted distributions.
    pub fn read_records(&self) -> Option<Vec<RawRecord>> {
        match self {
            Self::Lite(_) => None,
            Self::Full(b) => Some(b.read_records()),
        }
    }

    /// The stats level of this buffer.
    pub fn level(&self) -> StatsLevel {
        match self {
            Self::Lite(_) => StatsLevel::Lite,
            Self::Full(_) => StatsLevel::Full,
        }
    }

    /// Reset all counters and samples in this pad buffer.
    pub fn reset(&self) {
        match self {
            Self::Lite(b) => b.reset(),
            Self::Full(b) => b.reset(),
        }
    }
}

// ── Lite backend: atomic accumulators ──

/// Divisor applied to buffer sizes before squaring in the Lite variance
/// accumulator. Larger values prevent overflow at the cost of quantisation.
///
/// Headroom at 100 fps for 1 year of continuous recording:
///   - Raw 8K NV12 (~50 MB/frame): ~2.5 years
///   - H264 800 Mbps (~1 MB avg):  ~4 370 years
///   - H264 200 Mbps (~250 KB avg): ~98 000 years
const SIZE_COARSEN_DIVISOR: u64 = 1024;

/// Lock-free O(1) pad buffer using integrated (cumulative) accumulators.
///
/// Statistics are obtained by *differentiating* two snapshots taken at
/// different times:
///
/// ```text
/// mean_interval = (snap2.sum_interval_ns - snap1.sum_interval_ns)
///               / (snap2.total_buffers - snap1.total_buffers)
/// variance      = (sum_sq_diff / count_diff) - mean^2
/// throughput    = count_diff / dt
/// ```
pub struct LitePadBuffer {
    // ── Cumulative sums (for differentiation) ──
    /// Sum of inter-buffer intervals in nanoseconds.
    sum_interval_ns: AtomicU64,
    /// Sum of (interval_us)^2 for variance computation (units: µs^2).
    /// Stored in µs^2 rather than ns^2 to avoid overflow: at 10ms intervals
    /// each term is 10^8, giving ~58 years of headroom at 100fps (~17.5 years
    /// at 30fps).
    sum_interval_sq_us: AtomicU64,
    /// Sum of buffer sizes (bytes).
    sum_size_bytes: AtomicU64,
    /// Sum of (size / SIZE_COARSEN_DIVISOR)^2 for variance computation.
    /// Stored in coarsened units to avoid overflow with large frames.
    /// See [`SIZE_COARSEN_DIVISOR`] for overflow-headroom figures.
    sum_size_sq_units: AtomicU64,

    // ── Running min/max (atomic CAS) ──
    /// Running minimum interval (ns). Initialized to u64::MAX.
    min_interval_ns: AtomicU64,
    /// Running maximum interval (ns). Initialized to 0.
    max_interval_ns: AtomicU64,

    // ── Counters ──
    pub total_buffers: AtomicU64,
    pub total_keyframes: AtomicU64,
    pub total_delta_frames: AtomicU64,

    // ── Cross-element delay ──
    /// Wall-clock timestamp of the most recent buffer (ns since epoch).
    pub last_wall_ns: AtomicU64,
    /// Previous wall_ns for computing the interval in `record()`.
    prev_wall_ns: AtomicU64,
}

impl Default for LitePadBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LitePadBuffer {
    pub fn new() -> Self {
        Self {
            sum_interval_ns: AtomicU64::new(0),
            sum_interval_sq_us: AtomicU64::new(0),
            sum_size_bytes: AtomicU64::new(0),
            sum_size_sq_units: AtomicU64::new(0),
            min_interval_ns: AtomicU64::new(u64::MAX),
            max_interval_ns: AtomicU64::new(0),
            total_buffers: AtomicU64::new(0),
            total_keyframes: AtomicU64::new(0),
            total_delta_frames: AtomicU64::new(0),
            last_wall_ns: AtomicU64::new(0),
            prev_wall_ns: AtomicU64::new(0),
        }
    }

    /// Record a buffer observation. Fully lock-free.
    #[inline]
    pub fn record(&self, wall_ns: u64, size: u32, is_keyframe: bool) {
        let prev = self.prev_wall_ns.swap(wall_ns, Ordering::Relaxed);
        if prev > 0 {
            let interval = wall_ns.saturating_sub(prev);
            self.sum_interval_ns.fetch_add(interval, Ordering::Relaxed);
            // Accumulate (interval_us)^2 for variance. Converting ns -> µs
            // before squaring extends overflow headroom to ~58 years at 100fps
            // (~17.5 years at 30fps). Saturating mul protects against
            // pathological gaps.
            let interval_us = interval / 1000;
            self.sum_interval_sq_us
                .fetch_add(interval_us.saturating_mul(interval_us), Ordering::Relaxed);
            // Atomic CAS min/max
            atomic_fetch_min(&self.min_interval_ns, interval);
            atomic_fetch_max(&self.max_interval_ns, interval);
        }

        let sz = size as u64;
        self.sum_size_bytes.fetch_add(sz, Ordering::Relaxed);
        // Accumulate (size / SIZE_COARSEN_DIVISOR)^2 for variance.
        // See the constant definition for overflow-headroom figures.
        let sz_unit = sz / SIZE_COARSEN_DIVISOR;
        self.sum_size_sq_units
            .fetch_add(sz_unit.saturating_mul(sz_unit), Ordering::Relaxed);

        self.last_wall_ns.store(wall_ns, Ordering::Relaxed);
        self.total_buffers.fetch_add(1, Ordering::Relaxed);
        if is_keyframe {
            self.total_keyframes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.total_delta_frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Read the current accumulated state as a snapshot.
    pub(crate) fn snapshot(&self) -> InternalPadSnapshot {
        let total = self.total_buffers.load(Ordering::Relaxed);
        let kf = self.total_keyframes.load(Ordering::Relaxed);
        let df = self.total_delta_frames.load(Ordering::Relaxed);
        let sum_ns = self.sum_interval_ns.load(Ordering::Relaxed);
        let sum_sq_us = self.sum_interval_sq_us.load(Ordering::Relaxed);
        let sum_size = self.sum_size_bytes.load(Ordering::Relaxed);
        let sum_size_sq_units = self.sum_size_sq_units.load(Ordering::Relaxed);
        let min_ns = self.min_interval_ns.load(Ordering::Relaxed);
        let max_ns = self.max_interval_ns.load(Ordering::Relaxed);
        let last_wall = self.last_wall_ns.load(Ordering::Relaxed);

        // Number of intervals = total_buffers - 1 (first buffer has no interval)
        let interval_count = total.saturating_sub(1);

        let (mean_interval_ms, std_interval_ms) = if interval_count > 0 {
            let mean_ns = sum_ns as f64 / interval_count as f64;
            // Variance uses µs^2 units: convert mean_ns to µs for consistency.
            let mean_us = mean_ns / 1000.0;
            let mean_sq_us = sum_sq_us as f64 / interval_count as f64;
            let var_us = (mean_sq_us - mean_us * mean_us).max(0.0);
            (mean_ns / 1e6, var_us.sqrt() / 1000.0)
        } else {
            (0.0, 0.0)
        };

        let (mean_size_bytes, std_size_bytes) = if total > 0 {
            let mean_bytes = sum_size as f64 / total as f64;
            // Variance uses (bytes / SIZE_COARSEN_DIVISOR)^2 units.
            let d = SIZE_COARSEN_DIVISOR as f64;
            let mean_units = mean_bytes / d;
            let mean_sq_units = sum_size_sq_units as f64 / total as f64;
            let var_units = (mean_sq_units - mean_units * mean_units).max(0.0);
            (mean_bytes, var_units.sqrt() * d)
        } else {
            (0.0, 0.0)
        };

        InternalPadSnapshot {
            pad_name: Arc::from(""),
            level: StatsLevel::Lite,
            total_buffers: total,
            total_keyframes: kf,
            total_delta_frames: df,
            last_wall_ns: last_wall,

            // Lite aggregates
            accumulators: Some(AccumulatorSnapshot {
                sum_interval_ns: sum_ns,
                sum_interval_sq_us: sum_sq_us,
                sum_size_bytes: sum_size,
                sum_size_sq_units,
                interval_count,
                mean_interval_ms,
                std_interval_ms,
                min_interval_ms: if min_ns < u64::MAX {
                    min_ns as f64 / 1e6
                } else {
                    0.0
                },
                max_interval_ms: max_ns as f64 / 1e6,
                mean_size_bytes,
                std_size_bytes,
            }),

            // Full-only fields
            distribution: None,
            records: None,

            caps: None,
        }
    }

    /// Reset all accumulators and counters.
    pub fn reset(&self) {
        self.sum_interval_ns.store(0, Ordering::Relaxed);
        self.sum_interval_sq_us.store(0, Ordering::Relaxed);
        self.sum_size_bytes.store(0, Ordering::Relaxed);
        self.sum_size_sq_units.store(0, Ordering::Relaxed);
        self.min_interval_ns.store(u64::MAX, Ordering::Relaxed);
        self.max_interval_ns.store(0, Ordering::Relaxed);
        self.total_buffers.store(0, Ordering::Relaxed);
        self.total_keyframes.store(0, Ordering::Relaxed);
        self.total_delta_frames.store(0, Ordering::Relaxed);
        self.last_wall_ns.store(0, Ordering::Relaxed);
        self.prev_wall_ns.store(0, Ordering::Relaxed);
    }
}

// ── Full backend: atomic ring buffer ──

/// A single raw observation in the ring buffer.
/// All fields are atomic for lock-free single-producer writes.
#[repr(C)]
struct BufferRecord {
    wall_ns: AtomicU64,
    pts_ns: AtomicU64,
    size: AtomicU32,
    /// Bit 0: is_keyframe.
    flags: AtomicU8,
}

impl BufferRecord {
    fn new_empty() -> Self {
        Self {
            wall_ns: AtomicU64::new(0),
            pts_ns: AtomicU64::new(u64::MAX),
            size: AtomicU32::new(0),
            flags: AtomicU8::new(0),
        }
    }
}

/// Lock-free O(window) pad buffer storing raw observations in a ring.
///
/// The snapshot reader derives all statistics from the raw records:
/// intervals, percentiles, distributions, stutter counts, scatter data.
///
/// Torn reads at the write frontier are harmless — we compute aggregates
/// over hundreds of samples.
pub struct FullPadBuffer {
    ring: Box<[BufferRecord]>,
    /// Monotonically increasing write cursor. Slot = cursor % capacity.
    write_cursor: AtomicUsize,
    capacity: usize,

    // Same counters as Lite for consistency.
    pub total_buffers: AtomicU64,
    pub total_keyframes: AtomicU64,
    pub total_delta_frames: AtomicU64,
    pub last_wall_ns: AtomicU64,

    /// Combined cache for records + distributions.
    /// Uses `write_cursor` as the generation key — it already increments on
    /// every `record()`, so a dedicated generation counter is unnecessary.
    /// Records are wrapped in `Arc` so cache hits cost only an atomic increment
    /// instead of cloning ~21KB per pad.
    cached_snapshot: Mutex<Option<(usize, Arc<Vec<RawRecord>>, DistributionSnapshot)>>,
}

impl FullPadBuffer {
    pub fn new(capacity: usize) -> Self {
        let ring: Vec<BufferRecord> = (0..capacity).map(|_| BufferRecord::new_empty()).collect();
        Self {
            ring: ring.into_boxed_slice(),
            write_cursor: AtomicUsize::new(0),
            capacity,
            total_buffers: AtomicU64::new(0),
            total_keyframes: AtomicU64::new(0),
            total_delta_frames: AtomicU64::new(0),
            last_wall_ns: AtomicU64::new(0),
            cached_snapshot: Mutex::new(None),
        }
    }

    /// Record a buffer observation. Fully lock-free, wait-free.
    #[inline]
    pub fn record(&self, wall_ns: u64, pts_ns: Option<u64>, size: u32, is_keyframe: bool) {
        let idx = self.write_cursor.fetch_add(1, Ordering::Relaxed);
        let slot = &self.ring[idx % self.capacity];
        slot.wall_ns.store(wall_ns, Ordering::Relaxed);
        slot.pts_ns
            .store(pts_ns.unwrap_or(u64::MAX), Ordering::Relaxed);
        slot.size.store(size, Ordering::Relaxed);
        slot.flags
            .store(if is_keyframe { 1 } else { 0 }, Ordering::Relaxed);

        self.last_wall_ns.store(wall_ns, Ordering::Relaxed);
        self.total_buffers.fetch_add(1, Ordering::Relaxed);
        if is_keyframe {
            self.total_keyframes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.total_delta_frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Read the ring buffer contents as a Vec of (wall_ns, size, is_keyframe).
    /// Returns records in chronological order.
    pub fn read_records(&self) -> Vec<RawRecord> {
        let cursor = self.write_cursor.load(Ordering::Relaxed);
        if cursor == 0 {
            return Vec::new();
        }
        let start = cursor.saturating_sub(self.capacity);
        let cap = cursor - start;
        let mut records = Vec::with_capacity(cap);
        for i in start..cursor {
            let slot = &self.ring[i % self.capacity];
            let wall_ns = slot.wall_ns.load(Ordering::Relaxed);
            if wall_ns == 0 {
                continue;
            }
            let pts = slot.pts_ns.load(Ordering::Relaxed);
            records.push(RawRecord {
                wall_ns,
                pts_ns: if pts == u64::MAX { None } else { Some(pts) },
                size: slot.size.load(Ordering::Relaxed),
                is_keyframe: slot.flags.load(Ordering::Relaxed) & 1 != 0,
            });
        }
        records
    }

    /// Produce a snapshot with full distribution statistics derived from the ring.
    ///
    /// Uses `write_cursor` as a generation key to cache both records and
    /// distributions. On cache hit (cursor unchanged), cloning the cached
    /// records Vec (~21KB memcpy) is much faster than re-reading 900 atomic
    /// values from the ring buffer (36K atomic loads per pad).
    pub(crate) fn snapshot(&self) -> InternalPadSnapshot {
        let total = self.total_buffers.load(Ordering::Relaxed);
        let kf = self.total_keyframes.load(Ordering::Relaxed);
        let df = self.total_delta_frames.load(Ordering::Relaxed);
        let last_wall = self.last_wall_ns.load(Ordering::Relaxed);
        let current_gen = self.write_cursor.load(Ordering::Relaxed);

        let (records, distribution) = if let Ok(mut cache) = self.cached_snapshot.lock() {
            if let Some((cached_gen, ref recs, ref dist)) = *cache {
                if cached_gen == current_gen {
                    (Arc::clone(recs), dist.clone())
                } else {
                    let records = Arc::new(self.read_records());
                    let dist = Self::compute_distributions(&records);
                    *cache = Some((current_gen, Arc::clone(&records), dist.clone()));
                    (records, dist)
                }
            } else {
                let records = Arc::new(self.read_records());
                let dist = Self::compute_distributions(&records);
                *cache = Some((current_gen, Arc::clone(&records), dist.clone()));
                (records, dist)
            }
        } else {
            let records = Arc::new(self.read_records());
            let dist = Self::compute_distributions(&records);
            (records, dist)
        };

        InternalPadSnapshot {
            pad_name: Arc::from(""),
            level: StatsLevel::Full,
            total_buffers: total,
            total_keyframes: kf,
            total_delta_frames: df,
            last_wall_ns: last_wall,

            accumulators: None,
            distribution: Some(distribution),
            records: Some(records),

            caps: None,
        }
    }

    /// Compute all 6 distribution metrics from raw records in a single pass.
    fn compute_distributions(records: &[RawRecord]) -> DistributionSnapshot {
        let n = records.len();
        let interval_cap = n.saturating_sub(1);
        let mut intervals_ms = Vec::with_capacity(interval_cap);
        let mut i_intervals = Vec::with_capacity(interval_cap);
        let mut p_intervals = Vec::with_capacity(interval_cap);
        let mut sizes_bytes = Vec::with_capacity(n);
        let mut i_sizes = Vec::with_capacity(n);
        let mut p_sizes = Vec::with_capacity(n);

        for (idx, rec) in records.iter().enumerate() {
            let sz = rec.size as f64;
            sizes_bytes.push(sz);
            if rec.is_keyframe {
                i_sizes.push(sz);
            } else {
                p_sizes.push(sz);
            }

            if idx > 0 {
                let interval = (rec.wall_ns.saturating_sub(records[idx - 1].wall_ns)) as f64 / 1e6;
                intervals_ms.push(interval);
                if rec.is_keyframe {
                    i_intervals.push(interval);
                } else {
                    p_intervals.push(interval);
                }
            }
        }

        DistributionSnapshot {
            interval: Distribution::compute(&mut intervals_ms),
            i_interval: Distribution::compute(&mut i_intervals),
            p_interval: Distribution::compute(&mut p_intervals),
            size: Distribution::compute(&mut sizes_bytes),
            i_size: Distribution::compute(&mut i_sizes),
            p_size: Distribution::compute(&mut p_sizes),
        }
    }

    /// Reset counters and clear ring contents.
    pub fn reset(&self) {
        self.write_cursor.store(0, Ordering::Relaxed);
        self.total_buffers.store(0, Ordering::Relaxed);
        self.total_keyframes.store(0, Ordering::Relaxed);
        self.total_delta_frames.store(0, Ordering::Relaxed);
        self.last_wall_ns.store(0, Ordering::Relaxed);
        for slot in self.ring.iter() {
            slot.wall_ns.store(0, Ordering::Relaxed);
            slot.pts_ns.store(u64::MAX, Ordering::Relaxed);
            slot.size.store(0, Ordering::Relaxed);
            slot.flags.store(0, Ordering::Relaxed);
        }
        if let Ok(mut cache) = self.cached_snapshot.lock() {
            *cache = None;
        }
    }
}

// ── ElementProbe: per-element container ──

/// Per-element measurement container with dynamically registered pad buffers.
///
/// Pad buffers are created on demand via `register_sink_pad()` /
/// `register_src_pad()` and stored in per-direction maps keyed by pad name.
/// Each pad gets its own `Arc<PadBuffer>` that the GStreamer probe callback
/// captures directly — no lock on the hot path.
///
/// For tee elements, `skip_src_pads` is set so that dynamic request src pads
/// are never probed (they carry duplicate data).
pub struct ElementProbe {
    /// GStreamer element name (e.g. "rtph264depay0").
    pub element_name: Arc<str>,
    /// GStreamer element factory/type name (e.g. "rtph264depay").
    pub element_type: Arc<str>,
    /// Per-pad buffers for sink pads, keyed by pad name.
    sink_pads: Mutex<HashMap<Arc<str>, Arc<PadBuffer>>>,
    /// Per-pad buffers for src pads, keyed by pad name.
    src_pads: Mutex<HashMap<Arc<str>, Arc<PadBuffer>>>,
    /// Stats level for creating new pad buffers.
    stats_level: StatsLevel,
    /// Window capacity for creating new pad buffers.
    window_size: usize,
    /// If true, src pads are not probed (used for tee elements).
    skip_src_pads: bool,
    /// Linux kernel thread ID of the streaming thread driving this element.
    /// Updated on each pad probe callback via `update_thread_id()`.
    thread_id: AtomicU32,
    /// Thread ID captured from sink-pad probes only.
    sink_thread_id: AtomicU32,
    /// Thread ID captured from src-pad probes only.
    src_thread_id: AtomicU32,

    /// Immediate parent bin name (`None` if direct child of the pipeline).
    parent_bin: Option<Arc<str>>,

    /// Wall-clock ns of the most recent buffer arrival on any sink pad.
    /// Written by sink-pad probes, read by src-pad probes to compute
    /// per-buffer causal processing time.
    last_sink_arrival_ns: AtomicU64,
    /// Accumulated processing time (ns) across all buffers in the current
    /// measurement window. Written by src-pad probes.
    proc_time_sum_ns: AtomicU64,
    /// Number of per-buffer processing time samples in the current window.
    proc_time_count: AtomicU64,
}

impl ElementProbe {
    pub fn new(
        element_name: String,
        element_type: String,
        level: StatsLevel,
        capacity: usize,
        skip_src_pads: bool,
        parent_bin: Option<Arc<str>>,
    ) -> Self {
        Self {
            element_name: Arc::from(element_name),
            element_type: Arc::from(element_type),
            sink_pads: Mutex::new(HashMap::new()),
            src_pads: Mutex::new(HashMap::new()),
            stats_level: level,
            window_size: capacity,
            skip_src_pads,
            parent_bin,
            thread_id: AtomicU32::new(0),
            sink_thread_id: AtomicU32::new(0),
            src_thread_id: AtomicU32::new(0),
            last_sink_arrival_ns: AtomicU64::new(0),
            proc_time_sum_ns: AtomicU64::new(0),
            proc_time_count: AtomicU64::new(0),
        }
    }

    /// Record that a buffer arrived on a sink pad at `wall_ns`.
    /// Called from GStreamer streaming thread sink-pad probes.
    #[inline]
    pub fn record_sink_arrival(&self, wall_ns: u64) {
        self.last_sink_arrival_ns.store(wall_ns, Ordering::Relaxed);
    }

    /// Record that a buffer departed from a src pad at `wall_ns`.
    /// Computes the per-buffer processing time as `wall_ns - last_sink_arrival_ns`
    /// and accumulates it for the current measurement window.
    /// Called from GStreamer streaming thread src-pad probes.
    #[inline]
    pub fn record_src_departure(&self, wall_ns: u64) {
        let sink_ns = self.last_sink_arrival_ns.load(Ordering::Relaxed);
        if sink_ns > 0 && wall_ns > sink_ns {
            let delta = wall_ns - sink_ns;
            self.proc_time_sum_ns.fetch_add(delta, Ordering::Relaxed);
            self.proc_time_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Returns `true` if sink and src pads are driven by different threads.
    /// Only meaningful after at least one buffer has flowed through both sides.
    pub fn is_cross_thread(&self) -> Option<bool> {
        let sink_tid = self.sink_thread_id.load(Ordering::Relaxed);
        let src_tid = self.src_thread_id.load(Ordering::Relaxed);
        if sink_tid > 0 && src_tid > 0 {
            Some(sink_tid != src_tid)
        } else {
            None
        }
    }

    /// Update thread IDs from the calling thread. Stores the generic thread_id
    /// plus the direction-specific thread_id for cross-thread detection.
    /// Uses a thread-local cached `gettid()` on Linux. No-op on other platforms.
    #[inline]
    pub fn update_thread_id(&self, is_src: bool) {
        #[cfg(target_os = "linux")]
        {
            thread_local! {
                static CACHED_TID: u32 =
                    unsafe { libc::syscall(libc::SYS_gettid) } as u32;
            }
            let tid = CACHED_TID.with(|&t| t);
            self.thread_id.store(tid, Ordering::Relaxed);
            if is_src {
                self.src_thread_id.store(tid, Ordering::Relaxed);
            } else {
                self.sink_thread_id.store(tid, Ordering::Relaxed);
            }
        }
    }

    /// Get the current thread ID (0 if not yet captured).
    #[inline]
    pub fn get_thread_id(&self) -> u32 {
        self.thread_id.load(Ordering::Relaxed)
    }

    /// Lightweight read for CPU attribution: returns `(thread_id, processing_time_us)`.
    ///
    /// Only reads three atomics — much cheaper than a full `snapshot()`.
    /// Used by the 1 Hz CPU poll cycle to build element CPU history.
    #[inline]
    pub fn cpu_attribution_hint(&self) -> (Option<u32>, Option<f64>) {
        let tid = self.thread_id.load(Ordering::Relaxed);
        let thread_id = if tid > 0 { Some(tid) } else { None };

        let count = self.proc_time_count.load(Ordering::Relaxed);
        let sum = self.proc_time_sum_ns.load(Ordering::Relaxed);
        let processing_time_us = if count > 0 {
            Some((sum as f64 / count as f64) / 1000.0)
        } else {
            None
        };

        (thread_id, processing_time_us)
    }

    /// Register a new sink pad buffer. Returns the `Arc<PadBuffer>` for use
    /// in probe callbacks, or `None` if the pad is already registered.
    pub fn register_sink_pad(&self, pad_name: &str) -> Option<Arc<PadBuffer>> {
        let mut pads = self.sink_pads.lock().ok()?;
        if pads.contains_key(pad_name) {
            return None;
        }
        let buf = Arc::new(PadBuffer::new(self.stats_level, self.window_size));
        pads.insert(Arc::from(pad_name), Arc::clone(&buf));
        Some(buf)
    }

    /// Register a new src pad buffer. Returns `None` if src pads are skipped
    /// (tee elements) or the pad is already registered.
    pub fn register_src_pad(&self, pad_name: &str) -> Option<Arc<PadBuffer>> {
        if self.skip_src_pads {
            return None;
        }
        let mut pads = self.src_pads.lock().ok()?;
        if pads.contains_key(pad_name) {
            return None;
        }
        let buf = Arc::new(PadBuffer::new(self.stats_level, self.window_size));
        pads.insert(Arc::from(pad_name), Arc::clone(&buf));
        Some(buf)
    }

    /// Produce a snapshot of this element's measurements.
    /// Only includes pads that have recorded at least one buffer.
    pub(crate) fn snapshot(&self) -> InternalElementSnapshot {
        let sinks: Vec<InternalPadSnapshot> = self
            .sink_pads
            .lock()
            .ok()
            .map(|pads| {
                let mut out = Vec::with_capacity(pads.len());
                for (name, pb) in pads.iter() {
                    if pb.total_buffers() > 0 {
                        let mut snap = pb.snapshot();
                        snap.pad_name = name.clone();
                        out.push(snap);
                    }
                }
                out
            })
            .unwrap_or_default();

        let srcs: Vec<InternalPadSnapshot> = self
            .src_pads
            .lock()
            .ok()
            .map(|pads| {
                let mut out = Vec::with_capacity(pads.len());
                for (name, pb) in pads.iter() {
                    if pb.total_buffers() > 0 {
                        let mut snap = pb.snapshot();
                        snap.pad_name = name.clone();
                        out.push(snap);
                    }
                }
                out
            })
            .unwrap_or_default();

        let tid = self.thread_id.load(Ordering::Relaxed);

        let count = self.proc_time_count.load(Ordering::Relaxed);
        let processing_time_us = if count > 0 {
            let sum = self.proc_time_sum_ns.load(Ordering::Relaxed);
            Some(sum as f64 / count as f64 / 1_000.0)
        } else {
            None
        };

        InternalElementSnapshot {
            element_name: self.element_name.clone(),
            element_type: self.element_type.clone(),
            sink_pads: sinks,
            src_pads: srcs,
            thread_id: if tid > 0 { Some(tid) } else { None },
            parent_bin: self.parent_bin.clone(),
            processing_time_us,
            processing_time_stats: None, // Populated by PTS matching in Full mode
            estimated_cpu_pct: None,
            is_cross_thread: self.is_cross_thread(),
            state: None,
            properties: None,
            queue_stats: None,
        }
    }

    /// Read raw records from the best pad (most buffers), without computing
    /// distributions. Returns `(pad_name, records, total_buffers)` or `None`.
    /// This is the fast-path for the samples API.
    pub fn read_raw_records_from_best_pad(
        &self,
        pad_filter: Option<&str>,
    ) -> Option<(Arc<str>, Vec<RawRecord>, u64)> {
        let mut best: Option<(Arc<str>, Vec<RawRecord>, u64)> = None;

        // Check src pads first (preferred), then sink pads
        for pads in [self.src_pads.lock(), self.sink_pads.lock()]
            .into_iter()
            .flatten()
        {
            for (name, buf) in pads.iter() {
                if let Some(filter) = pad_filter {
                    if &**name != filter {
                        continue;
                    }
                }
                if let Some(records) = buf.read_records() {
                    let total = buf.total_buffers();
                    if best.as_ref().map(|(_, _, s)| total > *s).unwrap_or(true) {
                        best = Some((name.clone(), records, total));
                    }
                }
            }
        }

        best
    }

    /// Reset all registered pad buffers and cached thread ID.
    pub fn reset(&self) {
        if let Ok(pads) = self.sink_pads.lock() {
            for pad in pads.values() {
                pad.reset();
            }
        }
        if let Ok(pads) = self.src_pads.lock() {
            for pad in pads.values() {
                pad.reset();
            }
        }
        self.thread_id.store(0, Ordering::Relaxed);
        self.sink_thread_id.store(0, Ordering::Relaxed);
        self.src_thread_id.store(0, Ordering::Relaxed);
        self.last_sink_arrival_ns.store(0, Ordering::Relaxed);
        self.proc_time_sum_ns.store(0, Ordering::Relaxed);
        self.proc_time_count.store(0, Ordering::Relaxed);
    }
}

// Shared types (RawRecord, AccumulatorSnapshot, DistributionSnapshot,
// Distribution, StatsLevel) are defined in the `mcm_api::v1::stats` crate.
// Internal snapshot types (InternalPadSnapshot, InternalElementSnapshot)
// are defined above and converted to API types at the output boundary.

// ── Atomic helpers ──

/// Atomic fetch-min using CAS loop.
#[inline]
fn atomic_fetch_min(atom: &AtomicU64, val: u64) {
    let mut current = atom.load(Ordering::Relaxed);
    while val < current {
        match atom.compare_exchange_weak(current, val, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(c) => current = c,
        }
    }
}

/// Atomic fetch-max using CAS loop.
#[inline]
fn atomic_fetch_max(atom: &AtomicU64, val: u64) {
    let mut current = atom.load(Ordering::Relaxed);
    while val > current {
        match atom.compare_exchange_weak(current, val, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(c) => current = c,
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_basic_recording() {
        let buf = LitePadBuffer::new();

        // Simulate 5 buffers at ~33ms intervals
        let base_ns: u64 = 1_000_000_000; // 1s
        let interval_ns: u64 = 33_000_000; // 33ms

        for i in 0..5u64 {
            let wall = base_ns + i * interval_ns;
            buf.record(wall, 1024 * (i as u32 + 1), i == 0); // first is keyframe
        }

        assert_eq!(buf.total_buffers.load(Ordering::Relaxed), 5);
        assert_eq!(buf.total_keyframes.load(Ordering::Relaxed), 1);
        assert_eq!(buf.total_delta_frames.load(Ordering::Relaxed), 4);

        let snap = buf.snapshot();
        assert_eq!(snap.total_buffers, 5);
        let acc = snap.accumulators.unwrap();
        assert_eq!(acc.interval_count, 4); // 5 buffers = 4 intervals
        assert!((acc.mean_interval_ms - 33.0).abs() < 0.1);
        assert!(acc.std_interval_ms < 0.1); // all intervals identical
    }

    #[test]
    fn full_basic_recording() {
        let buf = FullPadBuffer::new(100);

        let base_ns: u64 = 1_000_000_000;
        let interval_ns: u64 = 33_000_000;

        for i in 0..5u64 {
            let wall = base_ns + i * interval_ns;
            buf.record(wall, None, 1024 * (i as u32 + 1), i == 0);
        }

        assert_eq!(buf.total_buffers.load(Ordering::Relaxed), 5);

        let records = buf.read_records();
        assert_eq!(records.len(), 5);
        assert!(records[0].is_keyframe);
        assert!(!records[1].is_keyframe);

        let snap = buf.snapshot();
        assert_eq!(snap.total_buffers, 5);
        let dist = snap.distribution.unwrap();
        assert_eq!(dist.interval.count, 4); // 4 intervals from 5 records
        assert!((dist.interval.mean - 33.0).abs() < 0.1);
    }

    #[test]
    fn full_ring_wraps_correctly() {
        let buf = FullPadBuffer::new(4); // tiny ring

        for i in 0..10u64 {
            buf.record(1_000_000_000 + i * 33_000_000, None, 1024, false);
        }

        // Should only have the last 4 records
        let records = buf.read_records();
        assert_eq!(records.len(), 4);
        // First record in ring should be from i=6
        let expected_first = 1_000_000_000 + 6 * 33_000_000;
        assert_eq!(records[0].wall_ns, expected_first);

        // But total counter should be 10
        assert_eq!(buf.total_buffers.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn lite_no_overflow_at_30fps_for_extended_run() {
        let buf = LitePadBuffer::new();

        // Simulate 20,000 buffers at 33ms intervals — well past the ~16,600
        // sample point where the old ns^2 accumulator would overflow u64.
        let base_ns: u64 = 1_000_000_000;
        let interval_ns: u64 = 33_000_000; // 33ms
        let sample_count: u64 = 20_000;

        for i in 0..sample_count {
            let wall = base_ns + i * interval_ns;
            buf.record(wall, 1024, false);
        }

        let snap = buf.snapshot();
        let acc = snap.accumulators.unwrap();

        // Mean should still be ~33ms
        assert!(
            (acc.mean_interval_ms - 33.0).abs() < 0.1,
            "mean_interval_ms drifted: {}",
            acc.mean_interval_ms
        );

        // All intervals are identical, so std should be near zero.
        // Before the fix, the old ns^2 accumulator would wrap around and
        // produce a wildly wrong std (thousands of ms).
        assert!(
            acc.std_interval_ms < 1.0,
            "std_interval_ms is corrupted (overflow?): {}",
            acc.std_interval_ms
        );
    }

    /// Verify that the size-variance accumulator has > 1 year headroom for
    /// raw 8K NV12 frames at 100 fps (worst-case per-frame size on a
    /// pre-encoder pad).
    #[test]
    fn lite_size_sq_headroom_raw_8k_at_100fps() {
        // 8K NV12: 7680 × 4320 × 1.5 bytes/pixel
        let raw_8k_size: u64 = 7680 * 4320 * 3 / 2; // 49_766_400
        let term =
            (raw_8k_size / SIZE_COARSEN_DIVISOR).saturating_mul(raw_8k_size / SIZE_COARSEN_DIVISOR);

        let fps: u64 = 100;
        let seconds_per_year: u64 = 365 * 24 * 3600 + 6 * 3600; // ~365.25 days
        let frames_per_year = fps * seconds_per_year;

        // Ensure per-year accumulation fits in u64 with margin.
        let total_per_year = (term as u128) * (frames_per_year as u128);
        let headroom_years = u64::MAX as u128 / total_per_year;
        assert!(
            headroom_years >= 1,
            "sum_size_sq_units would overflow in {headroom_years} year(s) \
             for raw 8K NV12 at {fps} fps (need >= 1)"
        );
    }

    /// Functional test: large 8K-sized frames produce sane variance stats
    /// without corruption.
    #[test]
    fn lite_variance_correct_with_large_frames() {
        let buf = LitePadBuffer::new();

        // Mix of I-frames (~5 MB) and P-frames (~500 KB) at 10ms intervals.
        let base_ns: u64 = 1_000_000_000;
        let interval_ns: u64 = 10_000_000; // 10ms (100fps)
        let i_size: u32 = 5_000_000;
        let p_size: u32 = 500_000;
        let gop = 30u64;
        let sample_count = 300u64; // 10 GOPs

        for i in 0..sample_count {
            let wall = base_ns + i * interval_ns;
            let is_kf = i % gop == 0;
            let sz = if is_kf { i_size } else { p_size };
            buf.record(wall, sz, is_kf);
        }

        let snap = buf.snapshot();
        let acc = snap.accumulators.unwrap();

        // Mean interval should be ~10 ms
        assert!(
            (acc.mean_interval_ms - 10.0).abs() < 0.1,
            "mean_interval_ms wrong: {}",
            acc.mean_interval_ms
        );

        // Mean size: (10 × 5 MB + 290 × 500 KB) / 300 = ~650 KB
        let expected_mean = (10.0 * 5_000_000.0 + 290.0 * 500_000.0) / 300.0;
        assert!(
            (acc.mean_size_bytes - expected_mean).abs() / expected_mean < 0.01,
            "mean_size_bytes wrong: {} (expected ~{})",
            acc.mean_size_bytes,
            expected_mean
        );

        // Std must be positive and reasonable (not corrupted by overflow).
        assert!(
            acc.std_size_bytes > 0.0 && acc.std_size_bytes < 5_000_000.0,
            "std_size_bytes looks corrupt: {}",
            acc.std_size_bytes
        );
    }

    #[test]
    fn pad_buffer_enum_dispatches() {
        let lite = PadBuffer::new(StatsLevel::Lite, 100);
        let full = PadBuffer::new(StatsLevel::Full, 100);

        assert_eq!(lite.level(), StatsLevel::Lite);
        assert_eq!(full.level(), StatsLevel::Full);

        lite.record(1_000_000_000, None, 1024, true);
        full.record(1_000_000_000, None, 1024, true);

        assert_eq!(lite.total_buffers(), 1);
        assert_eq!(full.total_buffers(), 1);
    }

    #[test]
    fn full_snapshot_cache_returns_same_result_when_unchanged() {
        let buf = FullPadBuffer::new(100);
        for i in 0..10u64 {
            buf.record(1_000_000_000 + i * 33_000_000, None, 1024, i == 0);
        }

        let snap1 = buf.snapshot();
        let snap2 = buf.snapshot();

        let d1 = snap1.distribution.unwrap();
        let d2 = snap2.distribution.unwrap();
        assert!((d1.interval.mean - d2.interval.mean).abs() < 1e-10);
        assert!((d1.size.mean - d2.size.mean).abs() < 1e-10);
        assert_eq!(d1.interval.count, d2.interval.count);
    }

    #[test]
    fn full_snapshot_cache_invalidated_on_new_record() {
        let buf = FullPadBuffer::new(100);
        for i in 0..5u64 {
            buf.record(1_000_000_000 + i * 33_000_000, None, 1024, false);
        }
        let snap1 = buf.snapshot();

        // Record a new observation with a different size
        buf.record(1_000_000_000 + 5 * 33_000_000, None, 2048, false);
        let snap2 = buf.snapshot();

        let d1 = snap1.distribution.unwrap();
        let d2 = snap2.distribution.unwrap();
        assert_ne!(d1.interval.count, d2.interval.count);
        assert_ne!(d1.size.count, d2.size.count);
    }

    #[test]
    fn full_snapshot_cache_cleared_on_reset() {
        let buf = FullPadBuffer::new(100);
        for i in 0..5u64 {
            buf.record(1_000_000_000 + i * 33_000_000, None, 1024, false);
        }
        let _snap1 = buf.snapshot(); // populate cache

        buf.reset();

        let snap2 = buf.snapshot();
        let d2 = snap2.distribution.unwrap();
        assert_eq!(d2.interval.count, 0);
        assert_eq!(d2.size.count, 0);
    }
}
