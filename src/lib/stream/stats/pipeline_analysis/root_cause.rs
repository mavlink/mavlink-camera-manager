use mcm_api::v1::stats::{IssueKind, PipelineSummary, RootCauseCandidate, ThreadBottleneck};
use rustc_hash::FxHashMap;

use super::{InternalEdgeDelay, InternalThreadGroup};
use crate::stream::stats::element_probe::InternalElementSnapshot;

/// Compute root cause candidates from internal computation data.
pub(crate) fn root_causes_for_internal(
    elements: &FxHashMap<String, InternalElementSnapshot>,
    edges: &[InternalEdgeDelay],
    expected_interval_ms: f64,
    pipeline_cpu_pct: Option<f64>,
) -> Vec<RootCauseCandidate> {
    let cpu = pipeline_cpu_pct.unwrap_or(0.0);
    let freshness: f64 = edges.iter().map(|e| e.freshness_delay_ms.max(0.0)).sum();
    let causal_score = edges
        .iter()
        .filter_map(|e| e.causal_match_rate)
        .map(|r| (1.0 - r).max(0.0))
        .fold(0.0_f64, f64::max)
        * 100.0;
    let freeze_score = elements
        .values()
        .flat_map(|el| el.src_pads.iter().chain(el.sink_pads.iter()))
        .map(|pad| {
            pad.accumulators
                .as_ref()
                .map(|a| a.max_interval_ms / expected_interval_ms)
                .unwrap_or(0.0)
                .max(
                    pad.distribution
                        .as_ref()
                        .map(|d| d.interval.max / expected_interval_ms)
                        .unwrap_or(0.0),
                )
        })
        .fold(0.0_f64, f64::max)
        * 10.0;

    let mut candidates = vec![
        RootCauseCandidate {
            cause: IssueKind::CpuSaturation,
            score: cpu,
        },
        RootCauseCandidate {
            cause: IssueKind::LatencySpike,
            score: freshness,
        },
        RootCauseCandidate {
            cause: IssueKind::CausalMatchLow,
            score: causal_score,
        },
        RootCauseCandidate {
            cause: IssueKind::FreezeRisk,
            score: freeze_score,
        },
    ];
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Compute thread bottlenecks from internal thread group data.
pub(crate) fn thread_bottlenecks_for_internal(
    thread_groups: &[InternalThreadGroup],
    summary: &PipelineSummary,
) -> Vec<ThreadBottleneck> {
    let latency = summary.total_pipeline_freshness_delay_ms;
    let mut out: Vec<ThreadBottleneck> = thread_groups
        .iter()
        .filter(|tg| tg.cpu_pct > 40.0)
        .map(|tg| ThreadBottleneck {
            thread_id: tg.thread_id,
            thread_name: tg.thread_name.clone(),
            cpu_pct: tg.cpu_pct,
            elements: tg.elements.clone(),
            latency_impact_ms: latency,
        })
        .collect();
    out.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}
