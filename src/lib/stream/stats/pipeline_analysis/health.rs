use mcm_api::v1::stats::{
    ElementSnapshot, FleetStats, HealthStatus, IssueKind, PipelineSnapshot, RootCauseCandidate,
    StreamSnapshot, StreamStats,
};

fn status_rank(status: HealthStatus) -> u8 {
    match status {
        HealthStatus::Good => 0,
        HealthStatus::Degraded => 1,
        HealthStatus::Bad => 2,
        HealthStatus::Unknown => 3,
    }
}

/// Collect all elements from a pipeline (recursing into bin children).
fn all_elements(snapshot: &PipelineSnapshot) -> Vec<&ElementSnapshot> {
    fn collect<'a>(elements: &'a [ElementSnapshot], out: &mut Vec<&'a ElementSnapshot>) {
        for elem in elements {
            out.extend(std::iter::once(elem));
            collect(&elem.children, out);
        }
    }
    let mut out = Vec::with_capacity(snapshot.elements.len() * 2);
    collect(&snapshot.elements, &mut out);
    out
}

/// Compute health status for a single pipeline from the new PipelineSnapshot.
pub(crate) fn pipeline_health(snapshot: &PipelineSnapshot) -> (HealthStatus, IssueKind) {
    let cpu = snapshot.stats.cpu_pct.unwrap_or(0.0);
    let freshness = snapshot.stats.summary.total_pipeline_freshness_delay_ms;

    let elements = all_elements(snapshot);

    let causal_low = elements
        .iter()
        .flat_map(|e| &e.connections)
        .filter_map(|c| c.causal_match_rate)
        .any(|r| r < 0.4);

    let expected = snapshot.stats.expected_interval_ms;
    let freeze_risk = elements
        .iter()
        .flat_map(|e| &e.pads)
        .any(|pad| {
            pad.stats
                .accumulators
                .as_ref()
                .map(|a| a.max_interval_ms > expected * 10.0)
                .unwrap_or(false)
                || pad
                    .stats
                    .distribution
                    .as_ref()
                    .map(|d| d.interval.max > expected * 10.0)
                    .unwrap_or(false)
        });

    if cpu > 95.0 {
        (HealthStatus::Bad, IssueKind::CpuSaturation)
    } else if freeze_risk {
        (HealthStatus::Bad, IssueKind::FreezeRisk)
    } else if freshness > 50.0 {
        (HealthStatus::Bad, IssueKind::LatencySpike)
    } else if cpu > 80.0 || freshness > 10.0 {
        (HealthStatus::Degraded, IssueKind::LatencySpike)
    } else if causal_low {
        (HealthStatus::Degraded, IssueKind::CausalMatchLow)
    } else {
        (HealthStatus::Good, IssueKind::Unknown)
    }
}

/// Compute aggregate stats for a stream from its pipelines.
///
/// Reads the pre-computed `health`/`dominant_issue` from each `PipelineStats`.
/// When `running` is false, the stream is marked as `Unknown` regardless of
/// pipeline data. When `running` is true but no pipeline analysis data exists
/// yet (transient state during startup), it is also marked `Unknown`.
pub(super) fn compute_stream_stats(pipelines: &[PipelineSnapshot], running: bool) -> StreamStats {
    if pipelines.is_empty() {
        return StreamStats {
            health: HealthStatus::Unknown,
            dominant_issue: IssueKind::Unknown,
            throughput_fps: 0.0,
            cpu_pct: 0.0,
            freshness_delay_ms: 0.0,
            root_cause_candidates: Vec::new(),
        };
    }

    let mut worst_health = if running {
        HealthStatus::Good
    } else {
        HealthStatus::Unknown
    };
    let mut worst_issue = IssueKind::Unknown;
    let mut total_fps = 0.0;
    let mut total_cpu = 0.0;
    let mut max_freshness = 0.0_f64;

    for p in pipelines {
        let h = p.stats.health;
        let i = p.stats.dominant_issue;
        if status_rank(h) > status_rank(worst_health) {
            worst_health = h;
            worst_issue = i;
        }
        total_fps += p.stats.summary.throughput_fps;
        total_cpu += p.stats.cpu_pct.unwrap_or(0.0);
        max_freshness = max_freshness.max(p.stats.summary.total_pipeline_freshness_delay_ms);
    }

    // Aggregate root cause candidates: merge from all pipelines, take highest scores.
    let root_cause_candidates: Vec<RootCauseCandidate> = if !pipelines.is_empty() {
        let mut merged: Vec<RootCauseCandidate> = pipelines
            .iter()
            .flat_map(|p| p.stats.root_cause_candidates.clone())
            .collect();
        // Deduplicate by cause, keeping highest score per cause.
        merged.sort_by(|a, b| {
            a.cause
                .partial_cmp(&b.cause)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        merged.dedup_by(|a, b| {
            if a.cause == b.cause {
                b.score = b.score.max(a.score);
                true
            } else {
                false
            }
        });
        merged.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        merged
    } else {
        Vec::new()
    };

    StreamStats {
        health: worst_health,
        dominant_issue: worst_issue,
        throughput_fps: total_fps,
        cpu_pct: total_cpu,
        freshness_delay_ms: max_freshness,
        root_cause_candidates,
    }
}

/// Compute fleet-level aggregate stats from all streams.
pub(super) fn compute_fleet_stats(streams: &[StreamSnapshot]) -> FleetStats {
    let mut good: u64 = 0;
    let mut degraded: u64 = 0;
    let mut bad: u64 = 0;
    let mut total_cpu = 0.0;
    let mut total_fps = 0.0;
    for s in streams {
        if s.running {
            match s.stats.health {
                HealthStatus::Good => good += 1,
                HealthStatus::Degraded => degraded += 1,
                HealthStatus::Bad | HealthStatus::Unknown => bad += 1,
            }
        }
        total_cpu += s.stats.cpu_pct;
        total_fps += s.stats.throughput_fps;
    }
    let overall = if bad > 0 {
        HealthStatus::Bad
    } else if degraded > 0 {
        HealthStatus::Degraded
    } else if streams.is_empty() {
        HealthStatus::Unknown
    } else {
        HealthStatus::Good
    };
    let dominant_issue = streams
        .iter()
        .max_by_key(|s| status_rank(s.stats.health))
        .map(|s| s.stats.dominant_issue)
        .unwrap_or(IssueKind::Unknown);

    FleetStats {
        overall_health: overall,
        streams_total: streams.len() as u64,
        streams_good: good,
        streams_degraded: degraded,
        streams_bad: bad,
        total_cpu_pct: total_cpu,
        total_throughput_fps: total_fps,
        dominant_issue,
    }
}
