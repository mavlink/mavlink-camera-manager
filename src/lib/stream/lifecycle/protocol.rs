use std::time::Duration;

use tokio::sync::oneshot;

use crate::stream::lifecycle::LifecycleSnapshot;

#[derive(Debug)]
pub(super) enum LifecycleCommand {
    AddConsumer {
        reply: oneshot::Sender<LifecycleSnapshot>,
    },
    RemoveConsumer {
        reply: oneshot::Sender<LifecycleSnapshot>,
    },
    AddConsumerNoReply,
    RemoveConsumerNoReply,
    PipelineReady {
        reply: oneshot::Sender<bool>,
    },
    PipelineError {
        reply: oneshot::Sender<Duration>,
    },
    DrainExpired {
        reply: oneshot::Sender<bool>,
    },
    ResetErrorBackoff {
        reply: oneshot::Sender<()>,
    },
    Shutdown,
}
