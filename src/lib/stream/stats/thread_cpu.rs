//! Per-thread CPU measurement via `/proc/self/task/{tid}/stat`.
//!
//! On Linux, each thread in a process has a stat file at
//! `/proc/self/task/{tid}/stat` containing cumulative CPU ticks (`utime` and
//! `stime`). By polling these periodically and computing deltas, we can
//! determine per-thread CPU usage percentage.
//!
//! On non-Linux platforms, all operations are no-ops and return empty results.

use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};

/// Per-thread CPU information snapshot.
#[derive(Debug, Clone)]
pub struct ThreadCpuInfo {
    /// Thread name from `/proc/self/task/{tid}/comm` (embedded in stat).
    pub thread_name: String,
    /// CPU usage percentage (user + system) since last poll.
    pub cpu_pct: f64,
}

/// Tracks per-thread CPU usage by periodically polling `/proc/self/task/{tid}/stat`.
pub struct ThreadCpuTracker {
    /// Previous cumulative ticks (utime + stime) per TID, for delta computation.
    prev_ticks: HashMap<u32, u64>,
    /// Latest CPU info per TID.
    current: HashMap<u32, ThreadCpuInfo>,
    /// Per-TID CPU usage history for windowed statistics.
    history: HashMap<u32, VecDeque<f64>>,
    history_capacity: usize,
    /// Clock ticks per second (`sysconf(_SC_CLK_TCK)`), typically 100 on Linux.
    ticks_per_sec: f64,
    /// Timestamp of last `poll()` call.
    last_poll: Option<Instant>,
}

impl Default for ThreadCpuTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadCpuTracker {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let tps = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        #[cfg(not(target_os = "linux"))]
        let tps = 100.0;

        Self {
            prev_ticks: HashMap::new(),
            current: HashMap::new(),
            history: HashMap::new(),
            history_capacity: 120,
            ticks_per_sec: tps,
            last_poll: None,
        }
    }

    /// Poll CPU usage for the given set of thread IDs.
    ///
    /// Reads `/proc/self/task/{tid}/stat` for each TID, computes delta CPU
    /// ticks since the last call, and converts to a CPU percentage.
    /// Stale entries (TIDs no longer in the set) are pruned.
    ///
    /// No-op on non-Linux platforms.
    pub fn poll(&mut self, tids: &[u32]) {
        #[cfg(target_os = "linux")]
        {
            let now = Instant::now();
            let elapsed_secs = self
                .last_poll
                .map(|prev| now.duration_since(prev).as_secs_f64())
                .unwrap_or(0.0);

            for &tid in tids {
                if let Some((utime, stime, name)) = read_thread_stat(tid) {
                    let total_ticks = utime + stime;
                    let cpu_pct = if elapsed_secs > 0.01 {
                        if let Some(&prev_total) = self.prev_ticks.get(&tid) {
                            let delta = total_ticks.saturating_sub(prev_total);
                            (delta as f64 / (elapsed_secs * self.ticks_per_sec)) * 100.0
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };
                    self.prev_ticks.insert(tid, total_ticks);
                    self.current.insert(
                        tid,
                        ThreadCpuInfo {
                            thread_name: name,
                            cpu_pct,
                        },
                    );

                    let hist = self
                        .history
                        .entry(tid)
                        .or_insert_with(|| VecDeque::with_capacity(self.history_capacity));
                    if hist.len() >= self.history_capacity {
                        hist.pop_front();
                    }
                    hist.push_back(cpu_pct);
                }
            }

            // Prune entries for TIDs no longer tracked.
            let tid_set: std::collections::HashSet<u32> = tids.iter().copied().collect();
            self.prev_ticks.retain(|tid, _| tid_set.contains(tid));
            self.current.retain(|tid, _| tid_set.contains(tid));
            self.history.retain(|tid, _| tid_set.contains(tid));
            self.last_poll = Some(now);
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = tids;
        }
    }

    /// Get the latest CPU info for a specific thread ID.
    pub fn get(&self, tid: u32) -> Option<&ThreadCpuInfo> {
        self.current.get(&tid)
    }

    /// Get all current thread CPU snapshots.
    pub fn all(&self) -> &HashMap<u32, ThreadCpuInfo> {
        &self.current
    }

    /// Get the CPU usage history for a specific thread.
    pub fn cpu_history(&self, tid: u32) -> Option<Vec<f64>> {
        self.history.get(&tid).map(|h| h.iter().copied().collect())
    }

    /// Reset all tracked thread state.
    pub fn reset(&mut self) {
        self.prev_ticks.clear();
        self.current.clear();
        self.history.clear();
        self.last_poll = None;
    }

    /// Insert a synthetic entry for testing. Not available in production builds.
    #[cfg(test)]
    pub fn insert_for_test(&mut self, tid: u32, thread_name: &str, cpu_pct: f64) {
        self.current.insert(
            tid,
            ThreadCpuInfo {
                thread_name: thread_name.to_string(),
                cpu_pct,
            },
        );
    }
}

/// Read `/proc/self/task/{tid}/stat` and extract `(utime, stime, comm)`.
///
/// The stat file format is:
/// ```text
/// pid (comm) state ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt utime stime ...
/// ```
/// The `comm` field is enclosed in parentheses and may contain spaces.
/// `utime` and `stime` are fields 14 and 15 (1-indexed), or indices 11 and 12
/// after splitting the part following the closing parenthesis.
#[cfg(target_os = "linux")]
fn read_thread_stat(tid: u32) -> Option<(u64, u64, String)> {
    let path = format!("/proc/self/task/{tid}/stat");
    let content = std::fs::read_to_string(path).ok()?;
    parse_thread_stat(&content)
}

/// Parse the content of a `/proc/self/task/{tid}/stat` file.
#[cfg(target_os = "linux")]
fn parse_thread_stat(content: &str) -> Option<(u64, u64, String)> {
    // comm is enclosed in parens and may contain spaces/parens.
    let open = content.find('(')?;
    let close = content.rfind(')')?;
    let comm = content[open + 1..close].to_string();
    let rest = content.get(close + 2..)?; // skip ") "
    let fields: Vec<&str> = rest.split_whitespace().collect();

    // After (comm): state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    //   flags(6) minflt(7) cminflt(8) majflt(9) cmajflt(10) utime(11) stime(12)
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime, stime, comm))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thread_stat() {
        // Realistic /proc/self/task/tid/stat content
        let content = "12345 (gst-streaming) S 12340 12345 12345 0 -1 1077944320 \
                        150 0 0 0 420 38 0 0 20 0 1 0 1234567 12345678 100 \
                        18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let result = parse_thread_stat(content);

        #[cfg(target_os = "linux")]
        {
            let (utime, stime, comm) = result.unwrap();
            assert_eq!(utime, 420);
            assert_eq!(stime, 38);
            assert_eq!(comm, "gst-streaming");
        }
    }

    #[test]
    fn test_parse_thread_stat_with_spaces_in_comm() {
        let content = "12345 (thread (main)) S 12340 12345 12345 0 -1 1077944320 \
                        150 0 0 0 100 50 0 0 20 0 1 0 1234567 12345678 100 \
                        18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let result = parse_thread_stat(content);

        #[cfg(target_os = "linux")]
        {
            let (utime, stime, comm) = result.unwrap();
            assert_eq!(utime, 100);
            assert_eq!(stime, 50);
            assert_eq!(comm, "thread (main)");
        }
    }

    #[test]
    fn test_tracker_new() {
        let tracker = ThreadCpuTracker::new();
        assert!(tracker.current.is_empty());
        assert!(tracker.prev_ticks.is_empty());
    }
}
