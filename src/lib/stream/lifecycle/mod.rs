use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::*;

mod actor;
mod protocol;
mod states;

const BACKOFF_CAP_SECS: u64 = 60;
const COMMAND_CHANNEL_CAPACITY: usize = 64;

use actor::LifecycleActor;
use protocol::LifecycleCommand;
use states::LifecycleState;

#[derive(Clone, Debug)]
pub struct LifecycleHandle {
    command_tx: mpsc::Sender<LifecycleCommand>,
    snapshot_rx: watch::Receiver<LifecycleSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleSnapshot {
    pub phase: Phase,

    pub consumers: u32,

    pub error_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,

    Waking,

    Running,

    Draining,
}

impl LifecycleHandle {
    pub fn lazy() -> Self {
        Self::spawn(LifecycleState::lazy())
    }

    pub fn eager() -> Self {
        Self::spawn(LifecycleState::eager())
    }

    fn spawn(state: LifecycleState) -> Self {
        let (snapshot_tx, snapshot_rx) = watch::channel(state.snapshot());
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        tokio::spawn(LifecycleActor::new(state, snapshot_tx).run(command_rx));
        Self {
            command_tx,
            snapshot_rx,
        }
    }

    pub fn snapshot(&self) -> LifecycleSnapshot {
        *self.snapshot_rx.borrow()
    }

    pub fn error_count(&self) -> u32 {
        self.snapshot().error_count
    }

    pub fn subscribe(&self) -> watch::Receiver<LifecycleSnapshot> {
        self.snapshot_rx.clone()
    }

    pub async fn add_consumer(&self) -> Result<LifecycleSnapshot> {
        self.request(|reply| LifecycleCommand::AddConsumer { reply })
            .await
    }

    pub fn add_consumer_in_background(&self) {
        if let Err(error) = self
            .command_tx
            .try_send(LifecycleCommand::AddConsumerNoReply)
        {
            warn!("Lifecycle background add_consumer dropped: {error}");
        }
    }

    pub async fn remove_consumer(&self) -> Result<LifecycleSnapshot> {
        self.request(|reply| LifecycleCommand::RemoveConsumer { reply })
            .await
    }

    pub fn remove_consumer_in_background(&self) {
        if let Err(error) = self
            .command_tx
            .try_send(LifecycleCommand::RemoveConsumerNoReply)
        {
            warn!("Lifecycle background remove_consumer dropped: {error}");
        }
    }

    pub async fn pipeline_ready(&self) -> Result<bool> {
        self.request(|reply| LifecycleCommand::PipelineReady { reply })
            .await
    }

    pub async fn pipeline_error(&self) -> Result<Duration> {
        self.request(|reply| LifecycleCommand::PipelineError { reply })
            .await
    }

    pub async fn drain_expired(&self) -> Result<bool> {
        self.request(|reply| LifecycleCommand::DrainExpired { reply })
            .await
    }

    pub async fn reset_error_backoff(&self) -> Result<()> {
        self.request(|reply| LifecycleCommand::ResetErrorBackoff { reply })
            .await
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.command_tx
            .send(LifecycleCommand::Shutdown)
            .await
            .map_err(|_| anyhow!("Lifecycle actor closed"))
    }

    pub fn shutdown_in_background(&self) {
        if let Err(error) = self.command_tx.try_send(LifecycleCommand::Shutdown) {
            debug!("Lifecycle background shutdown dropped: {error}");
        }
    }

    async fn request<T>(
        &self,
        make_command: impl FnOnce(oneshot::Sender<T>) -> LifecycleCommand,
    ) -> Result<T> {
        let (reply, receive) = oneshot::channel();
        self.command_tx
            .send(make_command(reply))
            .await
            .map_err(|_| anyhow!("Lifecycle actor closed"))?;
        receive.await.map_err(|_| anyhow!("Lifecycle actor closed"))
    }
}

pub(super) fn compute_backoff(consecutive_errors: u32) -> Duration {
    if consecutive_errors == 0 {
        return Duration::ZERO;
    }
    let secs = 1u64.checked_shl(consecutive_errors - 1).unwrap_or(u64::MAX);
    Duration::from_secs(secs.min(BACKOFF_CAP_SECS))
}

#[cfg(test)]
fn assert_snapshot(snapshot: LifecycleSnapshot, phase: Phase, consumers: u32, error_count: u32) {
    assert_eq!(
        snapshot,
        LifecycleSnapshot {
            phase,
            consumers,
            error_count,
        }
    );
    assert_snapshot_invariants(snapshot);
}

#[cfg(test)]
fn assert_snapshot_invariants(snapshot: LifecycleSnapshot) {
    match snapshot.phase {
        Phase::Idle | Phase::Draining => {
            assert_eq!(
                snapshot.consumers, 0,
                "{:?} cannot expose consumers",
                snapshot.phase
            );
        }
        Phase::Running => {
            assert!(
                snapshot.consumers > 0,
                "Running must have at least one consumer"
            );
        }
        Phase::Waking => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::time::{Duration as TokioDuration, timeout};

    use crate::stream::lifecycle::{
        BACKOFF_CAP_SECS, LifecycleHandle, Phase, assert_snapshot, assert_snapshot_invariants,
    };

    const BACKOFF_CAP: Duration = Duration::from_secs(BACKOFF_CAP_SECS);

    async fn wait_for_changed(rx: &mut watch::Receiver<LifecycleSnapshot>) -> LifecycleSnapshot {
        timeout(TokioDuration::from_secs(1), rx.changed())
            .await
            .expect("snapshot update timed out")
            .expect("snapshot channel closed");
        *rx.borrow()
    }

    async fn wait_for_snapshot(
        lifecycle: &LifecycleHandle,
        mut predicate: impl FnMut(LifecycleSnapshot) -> bool,
    ) -> LifecycleSnapshot {
        let mut rx = lifecycle.subscribe();
        let wait = async {
            loop {
                let snapshot = lifecycle.snapshot();
                if predicate(snapshot) {
                    return snapshot;
                }
                rx.changed().await.expect("snapshot channel closed");
            }
        };
        timeout(TokioDuration::from_secs(1), wait)
            .await
            .expect("snapshot predicate timed out")
    }

    #[tokio::test]
    async fn actor_subscriber_observes_updates() {
        let lifecycle = LifecycleHandle::lazy();
        let mut rx = lifecycle.subscribe();
        lifecycle.add_consumer().await.unwrap();
        let snapshot = wait_for_changed(&mut rx).await;
        assert_snapshot(snapshot, Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn actor_background_add_updates_snapshot() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer_in_background();
        let snapshot = wait_for_snapshot(&lifecycle, |snapshot| {
            snapshot.phase == Phase::Waking && snapshot.consumers == 1
        })
        .await;
        assert_snapshot(snapshot, Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn actor_background_remove_updates_snapshot() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.remove_consumer_in_background();
        let snapshot = wait_for_snapshot(&lifecycle, |snapshot| {
            snapshot.phase == Phase::Waking && snapshot.consumers == 0
        })
        .await;
        assert_snapshot(snapshot, Phase::Waking, 0, 0);
    }

    #[tokio::test]
    async fn actor_shutdown_closes_commands() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.shutdown().await.unwrap();
        assert!(lifecycle.add_consumer().await.is_err());
    }

    #[tokio::test]
    async fn actor_snapshot_publication_under_churn() {
        let lifecycle = LifecycleHandle::lazy();
        let mut rx = lifecycle.subscribe();
        let lifecycle_for_task = lifecycle.clone();
        let task = tokio::spawn(async move {
            for _ in 0..10 {
                lifecycle_for_task.add_consumer().await.unwrap();
            }
        });
        let observed = wait_for_changed(&mut rx).await;
        assert_snapshot_invariants(observed);
        task.await.unwrap();
        assert_eq!(*rx.borrow(), lifecycle.snapshot());
    }

    #[tokio::test]
    async fn lazy_actor_starts_idle_zero() {
        let lifecycle = LifecycleHandle::lazy();
        let snapshot = lifecycle.snapshot();
        assert_snapshot(snapshot, Phase::Idle, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_add_from_idle_publishes_waking() {
        let lifecycle = LifecycleHandle::lazy();
        let snapshot = lifecycle.add_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_add_from_waking_increments() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        let snapshot = lifecycle.add_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Waking, 2, 0);
    }

    #[tokio::test]
    async fn lazy_actor_add_from_running_increments() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        let snapshot = lifecycle.add_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Running, 2, 0);
    }

    #[tokio::test]
    async fn lazy_actor_add_from_draining_restores_running() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        lifecycle.remove_consumer().await.unwrap();
        let snapshot = lifecycle.add_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Running, 1, 0);
        assert!(!lifecycle.drain_expired().await.unwrap());
    }

    #[tokio::test]
    async fn lazy_actor_remove_from_idle_is_noop() {
        let lifecycle = LifecycleHandle::lazy();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Idle, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_remove_last_waking_stays_waking_zero() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Waking, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_remove_waking_decrements() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.add_consumer().await.unwrap();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_remove_running_decrements() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Running, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_remove_from_draining_is_noop() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        lifecycle.remove_consumer().await.unwrap();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Draining, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_ready_reaches_running() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        assert!(lifecycle.pipeline_ready().await.unwrap());
        let snapshot = lifecycle.snapshot();
        assert_snapshot(snapshot, Phase::Running, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_ready_waking_zero_drains() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.remove_consumer().await.unwrap();
        assert!(!lifecycle.pipeline_ready().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Draining, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_ready_outside_waking_is_noop() {
        let lifecycle = LifecycleHandle::lazy();
        assert!(!lifecycle.pipeline_ready().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Idle, 0, 0);

        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        assert!(!lifecycle.pipeline_ready().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Running, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_last_remove_drains_then_expires() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Draining, 0, 0);
        assert!(lifecycle.drain_expired().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Idle, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_drain_expired_outside_draining_is_noop() {
        let lifecycle = LifecycleHandle::lazy();
        assert!(!lifecycle.drain_expired().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Idle, 0, 0);

        lifecycle.add_consumer().await.unwrap();
        assert!(!lifecycle.drain_expired().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_error_idle_is_noop() {
        let lifecycle = LifecycleHandle::lazy();
        let backoff = lifecycle.pipeline_error().await.unwrap();
        assert_eq!(backoff, Duration::ZERO);
        assert_snapshot(lifecycle.snapshot(), Phase::Idle, 0, 0);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_error_waking_self_loop_increments_backoff() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        let first = lifecycle.pipeline_error().await.unwrap();
        let second = lifecycle.pipeline_error().await.unwrap();
        assert_eq!(first, Duration::from_secs(1));
        assert_eq!(second, Duration::from_secs(2));
        assert_snapshot(lifecycle.snapshot(), Phase::Waking, 1, 2);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_error_returns_backoff() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        let backoff = lifecycle.pipeline_error().await.unwrap();
        assert!(backoff > Duration::ZERO);
        let snapshot = lifecycle.snapshot();
        assert_snapshot(snapshot, Phase::Waking, 1, 1);
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_error_draining_goes_idle_with_backoff() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        lifecycle.remove_consumer().await.unwrap();
        let backoff = lifecycle.pipeline_error().await.unwrap();
        assert_eq!(backoff, Duration::from_secs(1));
        assert_snapshot(lifecycle.snapshot(), Phase::Idle, 0, 1);
    }

    #[tokio::test]
    async fn lazy_actor_drain_expired_preserves_error_count() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_error().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        lifecycle.remove_consumer().await.unwrap();
        assert!(lifecycle.drain_expired().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Idle, 0, 1);
    }

    #[tokio::test]
    async fn lazy_actor_reset_error_backoff_clears_errors() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_error().await.unwrap();
        lifecycle.reset_error_backoff().await.unwrap();
        assert_snapshot(lifecycle.snapshot(), Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn lazy_actor_backoff_caps() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        let mut backoff = Duration::ZERO;
        for _ in 0..100 {
            backoff = lifecycle.pipeline_error().await.unwrap();
        }
        assert_eq!(backoff, BACKOFF_CAP);
        assert_eq!(lifecycle.snapshot().error_count, 100);
    }

    #[tokio::test]
    async fn lazy_actor_concurrent_adds_are_serialized() {
        let lifecycle = LifecycleHandle::lazy();
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let lifecycle = lifecycle.clone();
            tasks.push(tokio::spawn(async move {
                lifecycle.add_consumer().await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let snapshot = lifecycle.snapshot();
        assert_snapshot(snapshot, Phase::Waking, 16, 0);
    }

    #[tokio::test]
    async fn lazy_actor_concurrent_add_remove_churn_preserves_invariants() {
        let lifecycle = LifecycleHandle::lazy();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let lifecycle = lifecycle.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..100 {
                    lifecycle.add_consumer().await.unwrap();
                }
            }));
        }
        for _ in 0..8 {
            let lifecycle = lifecycle.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..100 {
                    lifecycle.remove_consumer().await.unwrap();
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_snapshot_invariants(lifecycle.snapshot());
    }

    #[tokio::test]
    async fn lazy_actor_pipeline_error_racing_with_adds_serializes() {
        let lifecycle = LifecycleHandle::lazy();
        for _ in 0..4 {
            lifecycle.add_consumer().await.unwrap();
        }
        lifecycle.pipeline_ready().await.unwrap();

        let error_task = {
            let lifecycle = lifecycle.clone();
            tokio::spawn(async move {
                lifecycle.pipeline_error().await.unwrap();
            })
        };
        let mut add_tasks = Vec::new();
        for _ in 0..8 {
            let lifecycle = lifecycle.clone();
            add_tasks.push(tokio::spawn(async move {
                lifecycle.add_consumer().await.unwrap();
            }));
        }
        error_task.await.unwrap();
        for task in add_tasks {
            task.await.unwrap();
        }

        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.phase, Phase::Waking);
        assert_eq!(snapshot.consumers, 12);
        assert_eq!(snapshot.error_count, 1);
        assert_snapshot_invariants(snapshot);
    }

    #[tokio::test]
    async fn lazy_actor_drain_expiry_racing_with_reconnect_has_valid_serialized_outcome() {
        for _ in 0..100 {
            let lifecycle = LifecycleHandle::lazy();
            lifecycle.add_consumer().await.unwrap();
            lifecycle.pipeline_ready().await.unwrap();
            lifecycle.remove_consumer().await.unwrap();

            let expire = {
                let lifecycle = lifecycle.clone();
                tokio::spawn(async move { lifecycle.drain_expired().await.unwrap() })
            };
            let reconnect = {
                let lifecycle = lifecycle.clone();
                tokio::spawn(async move { lifecycle.add_consumer().await.unwrap() })
            };
            let expired = expire.await.unwrap();
            reconnect.await.unwrap();
            let snapshot = lifecycle.snapshot();

            if expired {
                assert_snapshot(snapshot, Phase::Waking, 1, 0);
            } else {
                assert_snapshot(snapshot, Phase::Running, 1, 0);
            }
        }
    }

    #[tokio::test]
    async fn eager_actor_starts_in_waking_with_phantom_consumer() {
        let lifecycle = LifecycleHandle::eager();
        assert_snapshot(lifecycle.snapshot(), Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn eager_actor_pipeline_ready_reaches_running() {
        let lifecycle = LifecycleHandle::eager();
        assert!(lifecycle.pipeline_ready().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Running, 1, 0);
    }

    #[tokio::test]
    async fn eager_actor_remove_last_consumer_clamps_at_phantom() {
        let lifecycle = LifecycleHandle::eager();
        lifecycle.pipeline_ready().await.unwrap();
        let snapshot = lifecycle.remove_consumer().await.unwrap();
        assert_snapshot(snapshot, Phase::Running, 1, 0);
    }

    #[tokio::test]
    async fn eager_actor_drain_expired_is_noop() {
        let lifecycle = LifecycleHandle::eager();
        assert!(!lifecycle.drain_expired().await.unwrap());
        assert_snapshot(lifecycle.snapshot(), Phase::Waking, 1, 0);
    }

    #[tokio::test]
    async fn eager_actor_pipeline_error_returns_to_waking() {
        let lifecycle = LifecycleHandle::eager();
        lifecycle.add_consumer().await.unwrap();
        lifecycle.pipeline_ready().await.unwrap();
        let backoff = lifecycle.pipeline_error().await.unwrap();
        assert!(backoff > Duration::ZERO);
        assert_snapshot(lifecycle.snapshot(), Phase::Waking, 2, 1);
    }

    #[tokio::test]
    async fn eager_actor_concurrent_remove_never_drops_below_one() {
        let lifecycle = LifecycleHandle::eager();
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let lifecycle = lifecycle.clone();
            tasks.push(tokio::spawn(async move {
                lifecycle.remove_consumer().await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let snapshot = lifecycle.snapshot();
        assert!(
            snapshot.consumers >= 1,
            "eager pipeline must never drop below the phantom consumer"
        );
        assert_eq!(snapshot.phase, Phase::Waking);
    }
}
