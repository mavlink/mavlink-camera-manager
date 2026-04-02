use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use super::types::StreamStatusState;

const PHASE_SHIFT: u32 = 30;
const COUNT_MASK: u32 = (1 << PHASE_SHIFT) - 1; // 0x3FFF_FFFF
pub const MAX_COUNT: u32 = COUNT_MASK;
const BACKOFF_CAP_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Idle = 0,
    Waking = 1,
    Running = 2,
    Draining = 3,
}

impl Phase {
    fn from_bits(bits: u32) -> Self {
        match bits & 0b11 {
            0 => Self::Idle,
            1 => Self::Waking,
            2 => Self::Running,
            3 => Self::Draining,
            _ => unreachable!(),
        }
    }
}

pub fn pack(phase: Phase, count: u32) -> u32 {
    (phase as u32) << PHASE_SHIFT | (count & COUNT_MASK)
}

pub fn unpack(v: u32) -> (Phase, u32) {
    (Phase::from_bits(v >> PHASE_SHIFT), v & COUNT_MASK)
}

pub trait NotifyHandle {
    fn notify_one(&self);
}

impl NotifyHandle for tokio::sync::Notify {
    fn notify_one(&self) {
        tokio::sync::Notify::notify_one(self);
    }
}

#[cfg(test)]
pub struct MockNotify {
    count: std::sync::Arc<AtomicU32>,
}

#[cfg(test)]
impl MockNotify {
    pub fn new() -> Self {
        Self {
            count: std::sync::Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn count(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }

    pub fn arc_count(&self) -> std::sync::Arc<AtomicU32> {
        self.count.clone()
    }
}

#[cfg(test)]
impl NotifyHandle for MockNotify {
    fn notify_one(&self) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }
}

pub struct LifecycleState {
    packed: AtomicU32,
    error_count: AtomicU32,
}

impl Default for LifecycleState {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleState {
    pub fn new() -> Self {
        Self {
            packed: AtomicU32::new(pack(Phase::Idle, 0)),
            error_count: AtomicU32::new(0),
        }
    }

    #[cfg(test)]
    pub fn from_raw(raw: u32) -> Self {
        Self {
            packed: AtomicU32::new(raw),
            error_count: AtomicU32::new(0),
        }
    }

    pub fn load(&self) -> (Phase, u32) {
        unpack(self.packed.load(Ordering::Acquire))
    }

    /// CAS loop: Idle→Waking(1)+notify, Draining→Running(1), else same phase count+1.
    pub fn add_consumer<N: NotifyHandle>(&self, notify: &N) -> u32 {
        loop {
            let cur = self.packed.load(Ordering::Acquire);
            let (phase, count) = unpack(cur);
            let new = match phase {
                Phase::Idle => pack(Phase::Waking, 1),
                Phase::Draining => pack(Phase::Running, 1),
                _ => pack(phase, count + 1),
            };
            if self
                .packed
                .compare_exchange_weak(cur, new, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if phase == Phase::Idle {
                    notify.notify_one();
                }
                return unpack(new).1;
            }
        }
    }

    /// CAS loop: decrements count. When last lazy consumer leaves Running→Draining,
    /// last lazy consumer leaves Waking→Waking(0). Guards against underflow (I6).
    pub fn remove_consumer(&self, is_lazy: bool) -> u32 {
        loop {
            let cur = self.packed.load(Ordering::Acquire);
            let (phase, count) = unpack(cur);
            if count == 0 {
                return 0;
            }
            let new = if count == 1 && is_lazy && matches!(phase, Phase::Running | Phase::Waking) {
                match phase {
                    Phase::Running => pack(Phase::Draining, 0),
                    Phase::Waking => pack(Phase::Waking, 0),
                    _ => unreachable!(),
                }
            } else {
                pack(phase, count - 1)
            };
            if self
                .packed
                .compare_exchange_weak(cur, new, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return unpack(new).1;
            }
        }
    }

    /// CAS loop: Waking(N>0)→Running(N), Waking(0)→Draining(0). Returns false
    /// if current phase is not Waking (no-op for Running/Idle/Draining).
    pub fn transition_to_running(&self) -> bool {
        loop {
            let cur = self.packed.load(Ordering::Acquire);
            let (phase, count) = unpack(cur);
            match phase {
                Phase::Waking => {
                    let new = if count > 0 {
                        pack(Phase::Running, count)
                    } else {
                        pack(Phase::Draining, 0)
                    };
                    if self
                        .packed
                        .compare_exchange_weak(cur, new, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return true;
                    }
                }
                _ => return false,
            }
        }
    }

    /// Single CAS: Draining(0)→Idle(0). Fails (returns false) if a consumer
    /// reconnected, guaranteeing mutual exclusion with add_consumer (I7).
    pub fn transition_to_idle(&self) -> bool {
        self.packed
            .compare_exchange(
                pack(Phase::Draining, 0),
                pack(Phase::Idle, 0),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Applies the pipeline-error transitions from the transition table:
    ///   Running(N)→Waking(N), Draining(0)→Idle(0), Waking(N)→Waking(N) (self-loop).
    /// Idle is a no-op. Increments error count (except from Idle) and returns
    /// the exponential backoff duration (1s→2s→4s→…→60s cap).
    pub fn handle_pipeline_error(&self) -> Duration {
        loop {
            let cur = self.packed.load(Ordering::Acquire);
            let (phase, count) = unpack(cur);
            match phase {
                Phase::Idle => return Duration::ZERO,
                Phase::Waking => {
                    self.error_count.fetch_add(1, Ordering::AcqRel);
                    return self.current_backoff();
                }
                Phase::Running => {
                    let new = pack(Phase::Waking, count);
                    if self
                        .packed
                        .compare_exchange_weak(cur, new, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.error_count.fetch_add(1, Ordering::AcqRel);
                        return self.current_backoff();
                    }
                }
                Phase::Draining => {
                    let new = pack(Phase::Idle, 0);
                    if self
                        .packed
                        .compare_exchange_weak(cur, new, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.error_count.fetch_add(1, Ordering::AcqRel);
                        return self.current_backoff();
                    }
                }
            }
        }
    }

    pub fn reset_error_backoff(&self) {
        self.error_count.store(0, Ordering::Release);
    }

    pub fn error_count(&self) -> u32 {
        self.error_count.load(Ordering::Acquire)
    }

    pub fn stream_status(&self) -> StreamStatusState {
        match self.load().0 {
            Phase::Idle | Phase::Draining => StreamStatusState::Idle,
            Phase::Waking | Phase::Running => StreamStatusState::Running,
        }
    }

    #[track_caller]
    pub fn assert_invariants(&self) {
        let (phase, count) = self.load();

        if count > 0 {
            assert!(
                matches!(phase, Phase::Waking | Phase::Running),
                "I1 violated: count={count} but phase={phase:?}"
            );
        }

        if phase == Phase::Idle {
            assert_eq!(count, 0, "I2 violated: Idle with count={count}");
        }

        if phase == Phase::Draining {
            assert_eq!(count, 0, "I3 violated: Draining with count={count}");
        }

        assert_ne!(
            self.stream_status(),
            StreamStatusState::Stopped,
            "I5 violated: status returned Stopped"
        );

        assert!(
            count <= MAX_COUNT,
            "I6 violated: count={count} exceeds MAX_COUNT"
        );
    }

    fn current_backoff(&self) -> Duration {
        Self::compute_backoff(self.error_count.load(Ordering::Acquire))
    }

    fn compute_backoff(consecutive_errors: u32) -> Duration {
        if consecutive_errors == 0 {
            return Duration::ZERO;
        }
        let secs = 1u64.checked_shl(consecutive_errors - 1).unwrap_or(u64::MAX);
        Duration::from_secs(secs.min(BACKOFF_CAP_SECS))
    }
}

impl std::fmt::Debug for LifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (phase, count) = self.load();
        f.debug_struct("LifecycleState")
            .field("phase", &phase)
            .field("count", &count)
            .field("error_count", &self.error_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a01_pack_unpack_roundtrip() {
        let phases = [Phase::Idle, Phase::Waking, Phase::Running, Phase::Draining];
        let counts = [0, 1, 42, MAX_COUNT];
        for &phase in &phases {
            for &count in &counts {
                let packed = pack(phase, count);
                let (p, c) = unpack(packed);
                assert_eq!(p, phase, "phase mismatch for {phase:?},{count}");
                assert_eq!(c, count, "count mismatch for {phase:?},{count}");
            }
        }
    }

    #[test]
    fn test_a02_default_constructor_idle_zero() {
        let ls = LifecycleState::new();
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Idle);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a03_add_from_idle_to_waking_notify_fired() {
        let ls = LifecycleState::new();
        let notify = MockNotify::new();
        let new_count = ls.add_consumer(&notify);
        assert_eq!(new_count, 1);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Waking);
        assert_eq!(count, 1);
        assert_eq!(notify.count(), 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_a04_add_from_waking_no_notify() {
        let ls = LifecycleState::from_raw(pack(Phase::Waking, 1));
        let notify = MockNotify::new();
        let new_count = ls.add_consumer(&notify);
        assert_eq!(new_count, 2);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Waking);
        assert_eq!(count, 2);
        assert_eq!(notify.count(), 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a05_add_from_running_increments() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 1));
        let notify = MockNotify::new();
        let new_count = ls.add_consumer(&notify);
        assert_eq!(new_count, 2);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 2);
        assert_eq!(notify.count(), 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a06_add_from_draining_to_running() {
        let ls = LifecycleState::from_raw(pack(Phase::Draining, 0));
        let notify = MockNotify::new();
        let new_count = ls.add_consumer(&notify);
        assert_eq!(new_count, 1);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 1);
        // Notify only fires on Idle→Waking, not Draining→Running
        assert_eq!(notify.count(), 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a07_remove_running_decrement_lazy() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 2));
        let new_count = ls.remove_consumer(true);
        assert_eq!(new_count, 1);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_a08_remove_last_running_to_draining_lazy() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 1));
        let new_count = ls.remove_consumer(true);
        assert_eq!(new_count, 0);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Draining);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a09_remove_nonlazy_keeps_running() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 1));
        let new_count = ls.remove_consumer(false);
        assert_eq!(new_count, 0);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_a10_remove_last_waking_stays_waking_lazy() {
        let ls = LifecycleState::from_raw(pack(Phase::Waking, 1));
        let new_count = ls.remove_consumer(true);
        assert_eq!(new_count, 0);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Waking);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_a11_remove_from_zero_noop() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 0));
        let new_count = ls.remove_consumer(true);
        assert_eq!(new_count, 0);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_a12_remove_from_idle_noop() {
        let ls = LifecycleState::new();
        let new_count = ls.remove_consumer(true);
        assert_eq!(new_count, 0);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Idle);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a13_transition_to_running_waking_with_consumers() {
        let ls = LifecycleState::from_raw(pack(Phase::Waking, 3));
        assert!(ls.transition_to_running());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 3);
        ls.assert_invariants();
    }

    #[test]
    fn test_a14_transition_to_running_waking_zero_goes_draining() {
        let ls = LifecycleState::from_raw(pack(Phase::Waking, 0));
        assert!(ls.transition_to_running());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Draining);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a15_transition_to_running_from_running_noop() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 3));
        assert!(!ls.transition_to_running());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 3);
        ls.assert_invariants();
    }

    #[test]
    fn test_a16_transition_to_running_from_idle_noop() {
        let ls = LifecycleState::new();
        assert!(!ls.transition_to_running());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Idle);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a17_transition_to_idle_from_draining() {
        let ls = LifecycleState::from_raw(pack(Phase::Draining, 0));
        assert!(ls.transition_to_idle());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Idle);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a18_transition_to_idle_fails_if_consumer_reconnected() {
        let ls = LifecycleState::from_raw(pack(Phase::Draining, 0));
        let notify = MockNotify::new();
        ls.add_consumer(&notify);
        assert!(!ls.transition_to_idle());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_a19_transition_to_idle_from_running_noop() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 3));
        assert!(!ls.transition_to_idle());
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 3);
        ls.assert_invariants();
    }

    #[test]
    fn test_a20_pipeline_error_draining_to_idle() {
        let ls = LifecycleState::from_raw(pack(Phase::Draining, 0));
        let backoff = ls.handle_pipeline_error();
        assert!(backoff > Duration::ZERO);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Idle);
        assert_eq!(count, 0);
        assert_eq!(ls.error_count(), 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_a21_pipeline_error_running_to_waking() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 3));
        let backoff = ls.handle_pipeline_error();
        assert!(backoff > Duration::ZERO);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Waking);
        assert_eq!(count, 3);
        assert_eq!(ls.error_count(), 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_a22_pipeline_error_waking_selfloop() {
        let ls = LifecycleState::from_raw(pack(Phase::Waking, 2));
        let backoff = ls.handle_pipeline_error();
        assert!(backoff > Duration::ZERO);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Waking);
        assert_eq!(count, 2);
        assert_eq!(ls.error_count(), 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_a23_pipeline_error_idle_noop() {
        let ls = LifecycleState::new();
        let backoff = ls.handle_pipeline_error();
        assert_eq!(backoff, Duration::ZERO);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Idle);
        assert_eq!(count, 0);
        assert_eq!(ls.error_count(), 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_a24_status_idle() {
        let ls = LifecycleState::new();
        assert_eq!(ls.stream_status(), StreamStatusState::Idle);
    }

    #[test]
    fn test_a25_status_draining_is_idle() {
        let ls = LifecycleState::from_raw(pack(Phase::Draining, 0));
        assert_eq!(ls.stream_status(), StreamStatusState::Idle);
    }

    #[test]
    fn test_a26_status_waking_is_running() {
        let ls = LifecycleState::from_raw(pack(Phase::Waking, 1));
        assert_eq!(ls.stream_status(), StreamStatusState::Running);
    }

    #[test]
    fn test_a27_status_running() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 3));
        assert_eq!(ls.stream_status(), StreamStatusState::Running);
    }

    #[test]
    fn test_a28_status_never_stopped_exhaustive() {
        for &phase in &[Phase::Idle, Phase::Waking, Phase::Running, Phase::Draining] {
            let count = match phase {
                Phase::Idle | Phase::Draining => 0,
                Phase::Waking | Phase::Running => 1,
            };
            let ls = LifecycleState::from_raw(pack(phase, count));
            assert_ne!(
                ls.stream_status(),
                StreamStatusState::Stopped,
                "I5 violated for {phase:?}"
            );
        }
    }

    #[test]
    fn test_a29_max_count_boundary() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, MAX_COUNT - 1));
        let notify = MockNotify::new();
        let new_count = ls.add_consumer(&notify);
        assert_eq!(new_count, MAX_COUNT);
        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, MAX_COUNT);
        ls.assert_invariants();
    }

    #[test]
    fn test_a30_full_lifecycle_sequence() {
        let ls = LifecycleState::new();
        let notify = MockNotify::new();

        ls.add_consumer(&notify);
        assert_eq!(ls.load(), (Phase::Waking, 1));
        ls.assert_invariants();

        assert!(ls.transition_to_running());
        assert_eq!(ls.load(), (Phase::Running, 1));
        ls.assert_invariants();

        ls.remove_consumer(true);
        assert_eq!(ls.load(), (Phase::Draining, 0));
        ls.assert_invariants();

        assert!(ls.transition_to_idle());
        assert_eq!(ls.load(), (Phase::Idle, 0));
        ls.assert_invariants();
    }

    #[test]
    fn test_b01_stress_concurrent_add_from_idle() {
        let ls = LifecycleState::new();
        let notify = MockNotify::new();
        let barrier = std::sync::Barrier::new(16);

        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    barrier.wait();
                    ls.add_consumer(&notify);
                });
            }
        });

        assert_eq!(ls.load(), (Phase::Waking, 16));
        assert_eq!(notify.count(), 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_b02_stress_concurrent_add_remove_churn() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 8));
        let notify = MockNotify::new();
        let barrier = std::sync::Barrier::new(16);

        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    barrier.wait();
                    for _ in 0..10_000 {
                        ls.add_consumer(&notify);
                    }
                });
            }
            for _ in 0..8 {
                s.spawn(|| {
                    barrier.wait();
                    for _ in 0..10_000 {
                        ls.remove_consumer(true);
                    }
                });
            }
        });

        let (_, count) = ls.load();
        assert!(count <= MAX_COUNT);
        ls.assert_invariants();
    }

    #[test]
    fn test_b03_stress_add_races_transition_to_idle() {
        for _ in 0..1_000 {
            let ls = LifecycleState::from_raw(pack(Phase::Draining, 0));
            let notify = MockNotify::new();
            let barrier = std::sync::Barrier::new(2);

            std::thread::scope(|s| {
                s.spawn(|| {
                    barrier.wait();
                    ls.add_consumer(&notify);
                });
                s.spawn(|| {
                    barrier.wait();
                    ls.transition_to_idle();
                });
            });

            let (phase, count) = ls.load();
            assert!(
                matches!((phase, count), (Phase::Running, 1) | (Phase::Waking, 1)),
                "I2/I7: unexpected ({phase:?}, {count})"
            );
            ls.assert_invariants();
        }
    }

    #[test]
    fn test_b04_stress_status_never_stopped_under_load() {
        let ls = LifecycleState::new();
        let notify = MockNotify::new();
        let barrier = std::sync::Barrier::new(5);
        let done = std::sync::atomic::AtomicBool::new(false);

        std::thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                for _ in 0..1_000 {
                    ls.add_consumer(&notify);
                    ls.transition_to_running();
                    ls.remove_consumer(true);
                    ls.transition_to_idle();
                }
                done.store(true, Ordering::Release);
            });
            for _ in 0..4 {
                s.spawn(|| {
                    barrier.wait();
                    while !done.load(Ordering::Acquire) {
                        assert_ne!(ls.stream_status(), StreamStatusState::Stopped);
                    }
                });
            }
        });

        ls.assert_invariants();
    }

    #[test]
    fn test_b05_stress_rapid_lifecycle_with_readers() {
        let ls = LifecycleState::new();
        let notify = MockNotify::new();
        let barrier = std::sync::Barrier::new(6);
        let writers_done = AtomicU32::new(0);

        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    barrier.wait();
                    for _ in 0..1_000 {
                        ls.add_consumer(&notify);
                        ls.transition_to_running();
                        ls.remove_consumer(true);
                        ls.transition_to_idle();
                    }
                    writers_done.fetch_add(1, Ordering::Release);
                });
            }
            for _ in 0..4 {
                s.spawn(|| {
                    barrier.wait();
                    while writers_done.load(Ordering::Acquire) < 2 {
                        let (phase, count) = ls.load();
                        if count > 0 {
                            assert!(
                                matches!(phase, Phase::Waking | Phase::Running),
                                "I1: count={count} but phase={phase:?}"
                            );
                        }
                        if phase == Phase::Idle {
                            assert_eq!(count, 0, "I2: Idle with count={count}");
                        }
                        if phase == Phase::Draining {
                            assert_eq!(count, 0, "I3: Draining with count={count}");
                        }
                    }
                });
            }
        });

        ls.assert_invariants();
    }

    #[test]
    fn test_b06_stress_no_underflow() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 0));
        let barrier = std::sync::Barrier::new(16);

        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    barrier.wait();
                    ls.remove_consumer(true);
                });
            }
        });

        let (phase, count) = ls.load();
        assert_eq!(phase, Phase::Running);
        assert_eq!(count, 0);
        ls.assert_invariants();
    }

    #[test]
    fn test_b07_stress_pipeline_error_during_add() {
        let ls = LifecycleState::from_raw(pack(Phase::Running, 4));
        let notify = MockNotify::new();
        let barrier = std::sync::Barrier::new(9);

        std::thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                ls.handle_pipeline_error();
            });
            for _ in 0..8 {
                s.spawn(|| {
                    barrier.wait();
                    ls.add_consumer(&notify);
                });
            }
        });

        let (phase, count) = ls.load();
        assert_eq!(count, 12);
        assert_eq!(phase, Phase::Waking);
        assert_eq!(ls.error_count(), 1);
        ls.assert_invariants();
    }

    #[test]
    fn test_b08_stress_draining_zero_never_persists() {
        for _ in 0..1_000 {
            let ls = LifecycleState::from_raw(pack(Phase::Running, 1));
            let notify = MockNotify::new();
            let barrier = std::sync::Barrier::new(5);

            std::thread::scope(|s| {
                s.spawn(|| {
                    barrier.wait();
                    ls.remove_consumer(true);
                });
                for _ in 0..4 {
                    s.spawn(|| {
                        barrier.wait();
                        ls.add_consumer(&notify);
                    });
                }
            });

            let (phase, count) = ls.load();
            assert_ne!(phase, Phase::Draining);
            assert_eq!(count, 4);
            ls.assert_invariants();
        }
    }

    // -- Property-based tests (C1–C3) ----------------------------------

    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Add,
        Remove { lazy: bool },
        TransitionToRunning,
        TransitionToIdle,
        PipelineError,
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            Just(Op::Add),
            any::<bool>().prop_map(|lazy| Op::Remove { lazy }),
            Just(Op::TransitionToRunning),
            Just(Op::TransitionToIdle),
            Just(Op::PipelineError),
        ]
    }

    fn apply_op(ls: &LifecycleState, notify: &MockNotify, op: &Op) {
        match op {
            Op::Add => {
                ls.add_consumer(notify);
            }
            Op::Remove { lazy } => {
                ls.remove_consumer(*lazy);
            }
            Op::TransitionToRunning => {
                ls.transition_to_running();
            }
            Op::TransitionToIdle => {
                ls.transition_to_idle();
            }
            Op::PipelineError => {
                ls.handle_pipeline_error();
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 1000, max_shrink_iters: 5000, ..Default::default() })]

        #[test]
        fn test_c01_prop_random_op_sequence(ops in prop::collection::vec(op_strategy(), 1..200)) {
            let ls = LifecycleState::new();
            let notify = MockNotify::new();
            for op in &ops {
                apply_op(&ls, &notify, op);
                ls.assert_invariants();
            }
        }

        #[test]
        fn test_c02_prop_status_consistency(ops in prop::collection::vec(op_strategy(), 1..200)) {
            let ls = LifecycleState::new();
            let notify = MockNotify::new();
            for op in &ops {
                apply_op(&ls, &notify, op);
                let (phase, _) = ls.load();
                let status = ls.stream_status();
                match phase {
                    Phase::Idle | Phase::Draining => assert_eq!(status, StreamStatusState::Idle),
                    Phase::Waking | Phase::Running => assert_eq!(status, StreamStatusState::Running),
                }
                assert_ne!(status, StreamStatusState::Stopped);
            }
        }

        #[test]
        fn test_c03_prop_random_concurrent_ops(
            thread_scripts in prop::collection::vec(
                prop::collection::vec(op_strategy(), 1..50),
                2..8usize,
            )
        ) {
            let ls = LifecycleState::new();
            let notify = MockNotify::new();
            let barrier = std::sync::Barrier::new(thread_scripts.len());

            let ls_ref = &ls;
            let notify_ref = &notify;
            let barrier_ref = &barrier;
            std::thread::scope(|s| {
                for script in &thread_scripts {
                    s.spawn(move || {
                        barrier_ref.wait();
                        for op in script {
                            apply_op(ls_ref, notify_ref, op);
                        }
                    });
                }
            });

            ls_ref.assert_invariants();
        }
    }
}
