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
