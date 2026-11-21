use mcm_api::v1::stats::HealthStatus;

use crate::stream::stats::element_probe::{InternalElementSnapshot, InternalPadSnapshot};

/// Compute stutter and freeze events from a single pad's data.
pub(crate) fn stutter_freeze_from_pad(
    pad: &InternalPadSnapshot,
    expected_interval_ms: f64,
    total_buffers_hint: u64,
) -> (u64, u64, f64, f64) {
    if expected_interval_ms <= 0.0 {
        return (0, 0, 0.0, 0.0);
    }
    let stutter_threshold = (expected_interval_ms * 2.0).max(expected_interval_ms + 20.0);
    let freeze_threshold = (expected_interval_ms * 10.0).max(500.0);

    // Fast path: if the max observed interval is below stutter threshold,
    // no record window can contain a stutter or freeze.
    let max_interval_hint = pad
        .accumulators
        .as_ref()
        .map(|a| a.max_interval_ms)
        .or_else(|| pad.distribution.as_ref().map(|d| d.interval.max))
        .unwrap_or(f64::MAX);
    if max_interval_hint < stutter_threshold {
        return (0, 0, 0.0, 0.0);
    }

    if let Some(records) = &pad.records {
        let mut stutter: u64 = 0;
        let mut freeze: u64 = 0;
        let mut max_freeze: f64 = 0.0;
        let mut interval_count: u64 = 0;

        for w in records.windows(2) {
            let interval = (w[1].wall_ns.saturating_sub(w[0].wall_ns)) as f64 / 1e6;
            interval_count += 1;
            if interval >= stutter_threshold {
                stutter += 1;
            }
            if interval >= freeze_threshold {
                freeze += 1;
                if interval > max_freeze {
                    max_freeze = interval;
                }
            }
        }

        let ratio = if interval_count == 0 {
            0.0
        } else {
            stutter as f64 / interval_count as f64
        };
        return (stutter, freeze, max_freeze, ratio);
    }

    let max_interval = pad
        .accumulators
        .as_ref()
        .map(|a| a.max_interval_ms)
        .unwrap_or(0.0);
    let stutter = if max_interval >= stutter_threshold {
        1
    } else {
        0
    };
    let freeze = if max_interval >= freeze_threshold {
        1
    } else {
        0
    };
    let ratio = if total_buffers_hint > 0 {
        stutter as f64 / total_buffers_hint as f64
    } else {
        0.0
    };
    (stutter, freeze, max_interval.max(0.0), ratio)
}

/// Compute element-level diagnostics from internal element data.
///
/// Returns `(health, stutter_events, freeze_events, max_freeze_ms, stutter_ratio)`.
pub(crate) fn element_diagnostics_from_internal(
    element: &InternalElementSnapshot,
    expected_interval_ms: f64,
) -> (HealthStatus, u64, u64, f64, f64) {
    let primary = element
        .src_pads
        .iter()
        .chain(element.sink_pads.iter())
        .max_by_key(|p| p.total_buffers);

    let Some(primary) = primary else {
        return (HealthStatus::Good, 0, 0, 0.0, 0.0);
    };

    let (stutter, freeze, max_freeze_ms, ratio) =
        stutter_freeze_from_pad(primary, expected_interval_ms, primary.total_buffers);

    let status = if freeze > 0 {
        HealthStatus::Bad
    } else if stutter > 0 {
        HealthStatus::Degraded
    } else {
        HealthStatus::Good
    };

    (status, stutter, freeze, max_freeze_ms, ratio)
}
