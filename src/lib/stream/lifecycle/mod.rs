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
}
