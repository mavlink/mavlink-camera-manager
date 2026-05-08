use tokio::sync::{mpsc, watch};

use crate::stream::lifecycle::{
    LifecycleSnapshot, protocol::LifecycleCommand, states::LifecycleState,
};

#[derive(Debug)]
pub(crate) struct LifecycleActor {
    state: LifecycleState,
    snapshot_tx: watch::Sender<LifecycleSnapshot>,
}

impl LifecycleActor {
    pub fn new(state: LifecycleState, snapshot_tx: watch::Sender<LifecycleSnapshot>) -> Self {
        Self { state, snapshot_tx }
    }

    #[tracing::instrument(level = "debug", skip(self, command_rx))]
    pub async fn run(mut self, mut command_rx: mpsc::Receiver<LifecycleCommand>) {
        while let Some(command) = command_rx.recv().await {
            if self.handle_command(command) {
                break;
            }
        }
    }

    #[tracing::instrument(level = "trace", skip(self, command), fields(?command))]
    fn handle_command(&mut self, command: LifecycleCommand) -> bool {
        match command {
            LifecycleCommand::AddConsumer { reply } => {
                self.state.add_consumer();
                self.publish();
                let _ = reply.send(self.state.snapshot());
            }
            LifecycleCommand::RemoveConsumer { reply } => {
                self.state.remove_consumer();
                self.publish();
                let _ = reply.send(self.state.snapshot());
            }
            LifecycleCommand::AddConsumerNoReply => {
                self.state.add_consumer();
                self.publish();
            }
            LifecycleCommand::RemoveConsumerNoReply => {
                self.state.remove_consumer();
                self.publish();
            }
            LifecycleCommand::PipelineReady { reply } => {
                let reached = self.state.pipeline_ready();
                self.publish();
                let _ = reply.send(reached);
            }
            LifecycleCommand::PipelineError { reply } => {
                let backoff = self.state.pipeline_error();
                self.publish();
                let _ = reply.send(backoff);
            }
            LifecycleCommand::DrainExpired { reply } => {
                let transitioned = self.state.drain_expired();
                self.publish();
                let _ = reply.send(transitioned);
            }
            LifecycleCommand::ResetErrorBackoff { reply } => {
                self.state.reset_error_backoff();
                self.publish();
                let _ = reply.send(());
            }
            LifecycleCommand::Shutdown => return true,
        }
        false
    }

    fn publish(&self) {
        let _ = self.snapshot_tx.send(self.state.snapshot());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::stream::lifecycle::{
        BACKOFF_CAP_SECS, LifecycleHandle, Phase, assert_snapshot, assert_snapshot_invariants,
    };

    const BACKOFF_CAP: Duration = Duration::from_secs(BACKOFF_CAP_SECS);

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
