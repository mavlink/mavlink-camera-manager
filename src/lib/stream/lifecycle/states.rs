use std::time::Duration;

use crate::stream::lifecycle::{LifecycleSnapshot, Phase, compute_backoff};

#[derive(Debug)]
pub(crate) struct LifecycleState {
    mode: Mode,
    phase: Phase,
    consumers: u32,
    error_count: u32,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Lazy,
    Eager,
}

impl LifecycleState {
    pub fn lazy() -> Self {
        Self {
            mode: Mode::Lazy,
            phase: Phase::Idle,
            consumers: 0,
            error_count: 0,
        }
    }

    pub fn eager() -> Self {
        Self {
            mode: Mode::Eager,
            phase: Phase::Waking,
            consumers: 1,
            error_count: 0,
        }
    }

    pub fn add_consumer(&mut self) {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::Waking;
                self.consumers = 1;
                self.error_count = 0;
            }
            Phase::Draining => {
                self.phase = Phase::Running;
                self.consumers = 1;
            }
            Phase::Waking | Phase::Running => {
                self.consumers = self.consumers.saturating_add(1);
            }
        }
    }

    pub fn remove_consumer(&mut self) {
        match self.phase {
            Phase::Idle | Phase::Draining => {}
            Phase::Waking => {
                self.consumers = self.consumers.saturating_sub(1);
                if matches!(self.mode, Mode::Eager) {
                    self.consumers = self.consumers.max(1);
                }
            }
            Phase::Running => {
                self.consumers = self.consumers.saturating_sub(1);
                match (self.mode, self.consumers) {
                    (Mode::Lazy, 0) => self.phase = Phase::Draining,
                    (Mode::Eager, 0) => self.consumers = 1,
                    _ => {}
                }
            }
        }
    }

    pub fn pipeline_ready(&mut self) -> bool {
        match self.phase {
            Phase::Waking if self.consumers > 0 => {
                self.phase = Phase::Running;
                true
            }
            Phase::Waking => {
                self.phase = Phase::Draining;
                false
            }
            _ => false,
        }
    }

    pub fn pipeline_error(&mut self) -> Duration {
        match self.phase {
            Phase::Idle => return Duration::ZERO,
            Phase::Draining => self.phase = Phase::Idle,
            Phase::Running => self.phase = Phase::Waking,
            Phase::Waking => {}
        }
        self.error_count = self.error_count.saturating_add(1);
        compute_backoff(self.error_count)
    }

    pub fn drain_expired(&mut self) -> bool {
        if matches!(self.phase, Phase::Draining) {
            self.phase = Phase::Idle;
            true
        } else {
            false
        }
    }

    pub fn reset_error_backoff(&mut self) {
        self.error_count = 0;
    }

    pub fn snapshot(&self) -> LifecycleSnapshot {
        let consumers = match self.phase {
            Phase::Idle | Phase::Draining => 0,
            Phase::Waking | Phase::Running => self.consumers,
        };
        LifecycleSnapshot {
            phase: self.phase,
            consumers,
            error_count: self.error_count,
        }
    }
}
