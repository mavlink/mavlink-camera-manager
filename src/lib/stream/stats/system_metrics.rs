/// System-level metrics snapshot (CPU, load, memory, temperature).
pub struct SystemMetrics {
    pub cpu_pct: f64,
    pub load_1m: f64,
    pub mem_used_pct: f64,
    pub temperature_c: f64,
}

/// Read system metrics once. Pass a mutable `prev_cpu` accumulator to enable
/// delta-based CPU usage calculation across consecutive calls.
pub fn snapshot(prev_cpu: &mut Option<(u64, u64)>) -> SystemMetrics {
    // CPU usage from /proc/stat (aggregate)
    let cpu_pct = (|| -> Option<f64> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let cpu_line = stat.lines().next()?;
        let vals: Vec<u64> = cpu_line
            .split_whitespace()
            .skip(1)
            .filter_map(|s| s.parse().ok())
            .collect();
        if vals.len() < 4 {
            return None;
        }
        let idle = vals[3] + vals.get(4).copied().unwrap_or(0); // idle + iowait
        let total: u64 = vals.iter().sum();
        let pct = if let Some((pi, pt)) = *prev_cpu {
            let dt = total.saturating_sub(pt);
            let di = idle.saturating_sub(pi);
            if dt > 0 {
                ((dt - di) as f64 / dt as f64) * 100.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        *prev_cpu = Some((idle, total));
        Some(pct)
    })()
    .unwrap_or(0.0);

    // Load average
    let loadavg_str = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let loads: Vec<f64> = loadavg_str
        .split_whitespace()
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();

    // Memory
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut mem_total_kb: u64 = 0;
    let mut mem_avail_kb: u64 = 0;
    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            mem_total_kb = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if line.starts_with("MemAvailable:") {
            mem_avail_kb = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }
    let mem_pct = if mem_total_kb > 0 {
        ((mem_total_kb - mem_avail_kb) as f64 / mem_total_kb as f64) * 100.0
    } else {
        0.0
    };

    // Temperature (RPi: thermal_zone0; generic: hwmon)
    let temp_c = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|t| t / 1000.0)
        .unwrap_or(0.0);

    SystemMetrics {
        cpu_pct,
        load_1m: loads.first().copied().unwrap_or(0.0),
        mem_used_pct: mem_pct,
        temperature_c: temp_c,
    }
}
