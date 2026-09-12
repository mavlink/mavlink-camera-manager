pub mod api;
pub mod gst_sender;
pub mod mcm;
pub mod poll;
pub mod timeouts;
pub mod types;

use std::sync::Once;

pub fn init_tracing() {
    static TRACING: Once = Once::new();
    TRACING.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    });
}
