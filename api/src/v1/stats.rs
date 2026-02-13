use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::v1::stream::{MavlinkComponent, VideoAndStreamInformation};

/// Instrumentation detail level, selectable via CLI / API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "lowercase")]
pub enum StatsLevel {
    Lite,
    Full,
}

impl std::fmt::Display for StatsLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lite => write!(f, "lite"),
            Self::Full => write!(f, "full"),
        }
    }
}

impl std::str::FromStr for StatsLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "lite" => Ok(Self::Lite),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown stats level: {other:?} (expected lite|full)"
            )),
        }
    }
}

/// A raw record from the Full ring buffer (exposed for scatter plots, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct RawRecord {
    #[ts(type = "number")]
    pub wall_ns: u64,
    /// Buffer PTS in nanoseconds when available.
    #[ts(type = "number")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_ns: Option<u64>,
    pub size: u32,
    pub is_keyframe: bool,
}

/// Snapshot of accumulated values from the Lite backend.
/// To compute rates, differentiate two successive snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct AccumulatorSnapshot {
    #[ts(type = "number")]
    pub sum_interval_ns: u64,
    /// Sum of (interval_us)^2 — microseconds squared to avoid u64 overflow.
    #[ts(type = "number")]
    pub sum_interval_sq_us: u64,
    #[ts(type = "number")]
    pub sum_size_bytes: u64,
    /// Sum of (size_bytes / 1024)^2 — coarsened to avoid u64 overflow with large frames.
    #[ts(type = "number")]
    pub sum_size_sq_units: u64,
    #[ts(type = "number")]
    pub interval_count: u64,
    /// Pre-computed from accumulators for convenience.
    pub mean_interval_ms: f64,
    pub std_interval_ms: f64,
    pub min_interval_ms: f64,
    pub max_interval_ms: f64,
    pub mean_size_bytes: f64,
    pub std_size_bytes: f64,
}

/// Statistical distribution summary.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct Distribution {
    #[ts(type = "number")]
    pub count: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub std: f64,
    pub median: f64,
    pub p95: f64,
    pub p99: f64,
}

impl Distribution {
    /// Compute distribution statistics using O(n) selection instead of O(n log n) sort.
    ///
    /// Uses `select_nth_unstable_by` to find median, p95, p99 without fully
    /// sorting the data. min/max/mean/std are computed in a single pass.
    pub fn compute(data: &mut [f64]) -> Self {
        if data.is_empty() {
            return Self::default();
        }
        let n = data.len();

        // Single pass for min, max, mean, variance
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0_f64;
        let mut sum_sq = 0.0_f64;
        for &v in data.iter() {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
            sum += v;
            sum_sq += v * v;
        }
        let mean = sum / n as f64;
        let var = (sum_sq / n as f64) - mean * mean;
        let std = if var > 0.0 { var.sqrt() } else { 0.0 };

        // Percentile indices (same formula as percentile_sorted)
        let median_idx = (0.50 * (n - 1) as f64).round() as usize;
        let p95_idx = (0.95 * (n - 1) as f64).round() as usize;
        let p99_idx = (0.99 * (n - 1) as f64).round() as usize;

        let cmp = |a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);

        // Cascaded selection: p99 first (partitions so [..p99] < data[p99] < [p99+1..])
        // Then p95 within [..=p99], then median within [..=p95].
        data.select_nth_unstable_by(p99_idx, cmp);
        let p99 = data[p99_idx];

        data[..=p99_idx].select_nth_unstable_by(p95_idx, cmp);
        let p95 = data[p95_idx];

        data[..=p95_idx].select_nth_unstable_by(median_idx, cmp);
        let median = data[median_idx];

        Self {
            count: n as u64,
            min,
            max,
            mean,
            std,
            median,
            p95,
            p99,
        }
    }

    /// Compute distribution statistics from an immutable slice (clones internally).
    ///
    /// Prefer [`compute`] when you own the data and can sort in place.
    pub fn from_slice(data: &[f64]) -> Self {
        if data.is_empty() {
            return Self::default();
        }
        let mut owned = data.to_vec();
        Self::compute(&mut owned)
    }
}

/// Full distribution statistics (from Full backend).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct DistributionSnapshot {
    pub interval: Distribution,
    pub i_interval: Distribution,
    pub p_interval: Distribution,
    pub size: Distribution,
    pub i_size: Distribution,
    pub p_size: Distribution,
}

/// Simple distribution for system metrics.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SystemDistribution {
    #[ts(type = "number")]
    pub count: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub std: f64,
}

impl SystemDistribution {
    pub fn from_slice(data: &[f64]) -> Self {
        if data.is_empty() {
            return Self::default();
        }
        let mut acc = SystemDistributionAccumulator::new();
        for &v in data {
            acc.push(v);
        }
        acc.finish()
    }
}

/// Online accumulator for [`SystemDistribution`] using Welford's algorithm.
///
/// Computes count, min, max, mean, and std in a single pass without
/// allocating any intermediate buffer.
pub struct SystemDistributionAccumulator {
    count: u64,
    mean: f64,
    m2: f64,
    min: f64,
    max: f64,
}

impl Default for SystemDistributionAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemDistributionAccumulator {
    pub fn new() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
            min: f64::MAX,
            max: f64::MIN,
        }
    }

    /// Feed one sample.
    #[inline]
    pub fn push(&mut self, v: f64) {
        self.count += 1;
        let delta = v - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = v - self.mean;
        self.m2 += delta * delta2;
        if v < self.min {
            self.min = v;
        }
        if v > self.max {
            self.max = v;
        }
    }

    /// Finalize into a [`SystemDistribution`].
    pub fn finish(self) -> SystemDistribution {
        if self.count == 0 {
            return SystemDistribution::default();
        }
        let var = self.m2 / self.count as f64;
        SystemDistribution {
            count: self.count,
            min: self.min,
            max: self.max,
            mean: self.mean,
            std: var.sqrt(),
        }
    }
}

/// System metrics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SystemSnapshot {
    #[ts(type = "number")]
    pub sample_count: u64,
    pub current_cpu_pct: f64,
    pub current_load_1m: f64,
    pub current_mem_used_pct: f64,
    pub current_temperature_c: f64,
    pub cpu_stats: SystemDistribution,
    pub load_stats: SystemDistribution,
    pub mem_stats: SystemDistribution,
    pub temp_stats: SystemDistribution,
}

/// Pipeline restart/uptime statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct RestartSnapshot {
    /// Total number of times this pipeline has been started (1 = first start, never restarted).
    #[ts(type = "number")]
    pub start_count: u64,
    /// Number of restarts (start_count - 1).
    #[ts(type = "number")]
    pub restart_count: u64,
    /// Seconds since the most recent start.
    pub current_uptime_secs: f64,
    /// Seconds since the very first start (total tracking time).
    pub total_tracked_secs: f64,
    /// Average seconds between restarts.
    pub avg_restart_interval_secs: f64,
    /// Minimum seconds between restarts (fastest crash loop).
    pub min_restart_interval_secs: f64,
    /// Seconds between the two most recent starts.
    pub last_restart_interval_secs: f64,
}

/// Delay measurement between two adjacent elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "lowercase")]
pub enum CausalConfidence {
    Low,
    Medium,
    High,
}

/// Aggregate pipeline health summary.
#[derive(Debug, Clone, Serialize, Deserialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PipelineSummary {
    /// Total frames observed at the most-active element.
    #[ts(type = "number")]
    pub total_frames: u64,
    /// Estimated throughput (frames/sec) from the most-active element.
    pub throughput_fps: f64,
    /// Sum of all inter-element freshness deltas (ms).
    pub total_pipeline_freshness_delay_ms: f64,
    /// Sum of per-edge causal latency along the pipeline's edges.
    /// Provides a stable pipeline-wide latency metric based on PTS-matched
    /// measurements rather than instantaneous freshness deltas.
    /// Only present when at least one edge has causal latency data.
    ///
    /// **Note on interpretation:** `mean` is exact (expectation of a sum = sum
    /// of expectations). `p95` and `p99` are conservative upper bounds (sum of
    /// per-edge percentiles >= true pipeline percentile). `min`/`max` are
    /// likewise sum-of-per-edge extremes. `count` is the minimum per-edge
    /// sample count (bottleneck edge), not the total. `std` is set to 0
    /// (computing it requires cross-edge covariance data we don't have).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pipeline_causal_latency_ms: Option<Distribution>,
    /// Aggregated confidence of causal edge latency measurements across
    /// this pipeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_latency_health: Option<CausalConfidence>,
    /// Human-readable health verdict.
    pub verdict: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Good,
    Degraded,
    Bad,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    CpuSaturation,
    FreezeRisk,
    LatencySpike,
    CausalMatchLow,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct RootCauseCandidate {
    pub cause: IssueKind,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ThreadBottleneck {
    pub thread_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
    pub cpu_pct: f64,
    pub elements: Vec<String>,
    pub latency_impact_ms: f64,
}

/// Request body for setting the analysis level.
#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SetLevelRequest {
    pub level: String,
}

/// Request body for setting the window size.
#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SetWindowSizeRequest {
    /// Allowed range is implementation-defined by server-side limits.
    #[ts(type = "number")]
    pub window_size: usize,
}

// ── New hierarchical model ──

/// Consolidated snapshot of all streams.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct StreamsSnapshot {
    #[ts(type = "number")]
    pub timestamp_ns: u64,
    pub stats: FleetStats,
    pub streams: Vec<StreamSnapshot>,
}

/// Fleet-level aggregate health statistics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct FleetStats {
    pub overall_health: HealthStatus,
    #[ts(type = "number")]
    pub streams_total: u64,
    #[ts(type = "number")]
    pub streams_good: u64,
    #[ts(type = "number")]
    pub streams_degraded: u64,
    #[ts(type = "number")]
    pub streams_bad: u64,
    pub total_cpu_pct: f64,
    pub total_throughput_fps: f64,
    pub dominant_issue: IssueKind,
}

/// Per-stream snapshot (aggregates across its pipelines).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct StreamSnapshot {
    pub id: String,
    pub name: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub video_and_stream: VideoAndStreamInformation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mavlink: Option<MavlinkComponent>,
    pub stats: StreamStats,
    pub pipelines: Vec<PipelineSnapshot>,
}

/// Stream-level aggregate statistics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct StreamStats {
    pub health: HealthStatus,
    pub dominant_issue: IssueKind,
    pub throughput_fps: f64,
    pub cpu_pct: f64,
    pub freshness_delay_ms: f64,
    pub root_cause_candidates: Vec<RootCauseCandidate>,
}

/// Per-pipeline snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PipelineSnapshot {
    pub name: String,
    pub stats: PipelineStats,
    pub connections: Vec<PipelineConnection>,
    /// Top-level elements (bins have their children nested inside).
    pub elements: Vec<ElementSnapshot>,
    /// Flat thread summary (elements reference threads via `thread_id`).
    pub threads: Vec<ThreadSummary>,
}

/// Pipeline-level statistics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PipelineStats {
    pub level: StatsLevel,
    #[ts(type = "number")]
    pub window_size: usize,
    pub expected_interval_ms: f64,
    pub uptime_secs: f64,
    pub health: HealthStatus,
    pub dominant_issue: IssueKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_stats: Option<SystemDistribution>,
    pub summary: PipelineSummary,
    pub system: SystemSnapshot,
    pub restarts: RestartSnapshot,
    pub root_cause_candidates: Vec<RootCauseCandidate>,
    pub thread_bottlenecks: Vec<ThreadBottleneck>,
}

/// Connection from one pipeline to another (e.g. proxy bridge).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PipelineConnection {
    pub to_pipeline: String,
    pub bridge_type: String,
    pub from_element: String,
    pub to_element: String,
}

/// Per-thread summary. Elements reference their thread via `ElementSnapshot.thread_id`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ThreadSummary {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub stats: ThreadStats,
    pub connections: Vec<ThreadConnection>,
}

/// Thread-level statistics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ThreadStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cpu_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_stats: Option<SystemDistribution>,
}

/// Connection from one thread to another (via a queue element).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ThreadConnection {
    pub to_thread: u32,
    pub via_element: String,
}

/// Per-element snapshot. When `is_bin` is true, this represents a GStreamer bin
/// and `children` contains its direct child elements (which may themselves be bins).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ElementSnapshot {
    pub name: String,
    pub element_type: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_bin: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ElementSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<ElementProperty>>,
    pub stats: ElementStats,
    pub connections: Vec<ElementConnection>,
    pub pads: Vec<PadSnapshot>,
}

/// Element-level statistics (includes inlined diagnostics).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ElementStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_time_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_time_stats: Option<Distribution>,
    /// `true` if the element's sink and src pads are driven by different
    /// threads (e.g. `queue`, `queue2`). When cross-thread, processing time
    /// includes queuing delay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_cross_thread: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_stats: Option<Distribution>,
    pub health: HealthStatus,
    #[ts(type = "number")]
    pub stutter_events: u64,
    #[ts(type = "number")]
    pub freeze_events: u64,
    pub max_freeze_ms: f64,
    pub stutter_ratio: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_stats: Option<QueueStats>,
}

/// Connection from one element to an adjacent downstream element.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ElementConnection {
    pub to_element: String,
    pub freshness_delay_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_latency_ms: Option<Distribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_match_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number")]
    pub causal_matched_samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_confidence: Option<CausalConfidence>,
}

/// Pad direction within an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "lowercase")]
pub enum PadDirection {
    Sink,
    Src,
}

/// Per-pad snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PadSnapshot {
    pub name: String,
    pub direction: PadDirection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps: Option<String>,
    pub stats: PadStats,
    /// Raw records from the ring buffer (Full mode only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buffer: Vec<RawRecord>,
    pub connections: Vec<PadConnection>,
}

/// Pad-level statistics.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PadStats {
    pub level: StatsLevel,
    #[ts(type = "number")]
    pub total_buffers: u64,
    #[ts(type = "number")]
    pub total_keyframes: u64,
    #[ts(type = "number")]
    pub total_delta_frames: u64,
    #[ts(type = "number")]
    pub total_dropped: u64,
    pub drop_ratio: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_gop_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_interval_ms: Option<Distribution>,
    #[ts(type = "number")]
    pub last_wall_ns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accumulators: Option<AccumulatorSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distribution: Option<DistributionSnapshot>,
}

/// Connection from one pad to its peer pad on an adjacent element.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PadConnection {
    pub peer_element: String,
    pub peer_pad: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// A single element property (name-value pair from GObject introspection).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ElementProperty {
    pub name: String,
    pub value: String,
}

/// Fill-level statistics for GStreamer queue/queue2 elements.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct QueueStats {
    pub current_level_buffers: u32,
    #[ts(type = "number")]
    pub current_level_bytes: u64,
    #[ts(type = "number")]
    pub current_level_time_ns: u64,
    pub max_level_buffers: u32,
    #[ts(type = "number")]
    pub max_level_bytes: u64,
    #[ts(type = "number")]
    pub max_level_time_ns: u64,
    pub fill_pct: f64,
}

/// Query parameters for the snapshot endpoint.
#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SnapshotQuery {
    /// Maximum number of raw records to include per pad.
    /// Defaults to 0 (omit raw buffer).
    #[serde(default)]
    #[ts(type = "number")]
    pub buffer_limit: usize,
}

fn default_ws_interval_ms() -> u64 {
    1000
}

/// Query parameters for the snapshot WebSocket endpoint.
#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SnapshotWsQuery {
    /// Push interval in milliseconds. Minimum 500, default 1000.
    #[serde(default = "default_ws_interval_ms")]
    #[ts(type = "number")]
    pub interval_ms: u64,
    /// Maximum number of raw records to include per pad.
    #[serde(default)]
    #[ts(type = "number")]
    pub buffer_limit: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_empty() {
        let d = Distribution::compute(&mut []);
        assert_eq!(d.count, 0);
        assert_eq!(d.min, 0.0);
        assert_eq!(d.max, 0.0);
        assert_eq!(d.mean, 0.0);
    }

    #[test]
    fn distribution_single_value() {
        let mut data = [42.0];
        let d = Distribution::compute(&mut data);
        assert_eq!(d.count, 1);
        assert!((d.min - 42.0).abs() < 1e-10);
        assert!((d.max - 42.0).abs() < 1e-10);
        assert!((d.mean - 42.0).abs() < 1e-10);
        assert!((d.median - 42.0).abs() < 1e-10);
        assert!((d.p95 - 42.0).abs() < 1e-10);
        assert!((d.p99 - 42.0).abs() < 1e-10);
        assert!(d.std < 1e-10);
    }

    #[test]
    fn distribution_known_values() {
        let mut data: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let d = Distribution::compute(&mut data);
        assert_eq!(d.count, 100);
        assert!((d.min - 1.0).abs() < 1e-10);
        assert!((d.max - 100.0).abs() < 1e-10);
        assert!((d.mean - 50.5).abs() < 1e-10);
        assert!((d.median - 50.0).abs() < 1.1); // index 50 -> value ~50
        assert!((d.p95 - 95.0).abs() < 1.1);
        assert!((d.p99 - 99.0).abs() < 1.1);
    }

    #[test]
    fn distribution_from_slice_matches_compute() {
        let data: Vec<f64> = (1..=50).map(|i| i as f64 * 0.1).collect();
        let d1 = Distribution::from_slice(&data);
        let mut data2 = data.clone();
        let d2 = Distribution::compute(&mut data2);
        assert!((d1.mean - d2.mean).abs() < 1e-10);
        assert!((d1.min - d2.min).abs() < 1e-10);
        assert!((d1.max - d2.max).abs() < 1e-10);
        assert!((d1.median - d2.median).abs() < 1e-10);
        assert!((d1.p95 - d2.p95).abs() < 1e-10);
        assert!((d1.p99 - d2.p99).abs() < 1e-10);
    }

    #[test]
    fn distribution_large_dataset_correctness() {
        // Generate 900 values simulating frame intervals at ~30fps
        let mut data: Vec<f64> = (0..900)
            .map(|i| 33.3 + (i % 7) as f64 * 0.1 - 0.3)
            .collect();
        let d = Distribution::compute(&mut data);
        assert_eq!(d.count, 900);
        assert!(d.min <= d.median);
        assert!(d.median <= d.p95);
        assert!(d.p95 <= d.p99);
        assert!(d.p99 <= d.max);
        assert!(d.std > 0.0);
    }

    #[test]
    fn distribution_two_values() {
        let mut data = [10.0, 20.0];
        let d = Distribution::compute(&mut data);
        assert_eq!(d.count, 2);
        assert!((d.min - 10.0).abs() < 1e-10);
        assert!((d.max - 20.0).abs() < 1e-10);
        assert!((d.mean - 15.0).abs() < 1e-10);
        assert!((d.std - 5.0).abs() < 1e-10);
    }
}
