use tokio::sync::mpsc;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::zenoh::foxglove_messages::Log;

pub struct FoxgloveLayer {
    sender: mpsc::Sender<Log>,
}

impl FoxgloveLayer {
    pub fn new(sender: mpsc::Sender<Log>) -> Self {
        Self { sender }
    }
}

impl<S> tracing_subscriber::Layer<S> for FoxgloveLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut message = String::new();
        event.record(&mut MessageVisitor {
            message: &mut message,
        });

        let level = match *metadata.level() {
            tracing::Level::ERROR => crate::zenoh::foxglove_messages::Level::Error,
            tracing::Level::WARN => crate::zenoh::foxglove_messages::Level::Warning,
            tracing::Level::INFO => crate::zenoh::foxglove_messages::Level::Info,
            tracing::Level::DEBUG => crate::zenoh::foxglove_messages::Level::Debug,
            tracing::Level::TRACE => crate::zenoh::foxglove_messages::Level::Unknown,
        };

        let log = Log {
            timestamp: crate::zenoh::foxglove_messages::Timestamp::now(),
            level,
            message,
            name: env!("CARGO_PKG_NAME").to_string(),
            file: metadata.file().unwrap_or("?").to_string(),
            line: metadata.line().unwrap_or(0),
        };

        let _ = self.sender.send(log);
    }
}

struct MessageVisitor<'a> {
    message: &'a mut String,
}

impl<'a> tracing::field::Visit for MessageVisitor<'a> {
    fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.message, "{value:?}");
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            use std::fmt::Write;
            let _ = write!(self.message, "{}={}", field.name(), value);
        }
    }
}
