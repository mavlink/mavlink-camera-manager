use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use ringbuffer::{AllocRingBuffer, RingBuffer};
use tokio::sync::broadcast::{Receiver, Sender};
use tracing::{metadata::LevelFilter, *};
use tracing_log::LogTracer;
use tracing_subscriber::{
    fmt::{self, MakeWriter},
    layer::SubscriberExt,
    EnvFilter, Layer,
};

use crate::cli;

struct BroadcastWriter {
    sender: Sender<String>,
}

impl Write for BroadcastWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let message = String::from_utf8_lossy(buf).to_string();
        let _ = self.sender.send(message);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BroadcastMakeWriter {
    sender: Sender<String>,
}

impl<'a> MakeWriter<'a> for BroadcastMakeWriter {
    type Writer = BroadcastWriter;

    fn make_writer(&'a self) -> Self::Writer {
        BroadcastWriter {
            sender: self.sender.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Manager {
    pub process: Option<tokio::task::JoinHandle<()>>,
}

pub struct History {
    pub history: AllocRingBuffer<String>,
    pub sender: Sender<String>,
}

impl Default for History {
    fn default() -> Self {
        let (sender, _receiver) = tokio::sync::broadcast::channel(100);
        Self {
            history: AllocRingBuffer::new(10 * 1024),
            sender,
        }
    }
}

impl History {
    pub fn push(&mut self, message: String) {
        self.history.push(message.clone());
        let _ = self.sender.send(message);
    }

    pub fn subscribe(&self) -> (Receiver<String>, Vec<String>) {
        let reader = self.sender.subscribe();
        (reader, self.history.to_vec())
    }
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Default::default();
    pub static ref HISTORY: Arc<Mutex<History>> = Default::default();
}

// Start logger, should be done inside main
pub fn init() {
    // Redirect all logs from libs using "Log"
    LogTracer::init_with_filter(tracing::log::LevelFilter::Trace).expect("Failed to set logger");

    // Configure the console log
    let console_env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if cli::manager::is_verbose() {
            EnvFilter::new(LevelFilter::DEBUG.to_string())
        } else {
            EnvFilter::new(LevelFilter::INFO.to_string())
        }
    });

    let console_layer = fmt::Layer::new()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(fmt::format::FmtSpan::NONE)
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_filter(filter_unwanted_crates(console_env_filter));

    // Configure the file log
    let file_env_filter = if cli::manager::is_tracing() {
        EnvFilter::new(LevelFilter::TRACE.to_string())
    } else {
        EnvFilter::new(LevelFilter::DEBUG.to_string())
    };
    let dir = cli::manager::log_path();
    let file_appender = tracing_appender::rolling::hourly(dir, "mavlink-camera-manager.", ".log");
    let file_layer = fmt::Layer::new()
        .with_writer(file_appender)
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(fmt::format::FmtSpan::NONE)
        .with_target(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_filter(filter_unwanted_crates(file_env_filter));

    // Configure the server log
    let server_env_filter = if cli::manager::is_tracing() {
        EnvFilter::new(LevelFilter::TRACE.to_string())
    } else {
        EnvFilter::new(LevelFilter::DEBUG.to_string())
    };
    let (tx, mut rx) = tokio::sync::broadcast::channel(100);
    let server_layer = fmt::Layer::new()
        .with_writer(BroadcastMakeWriter { sender: tx.clone() })
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(fmt::format::FmtSpan::NONE)
        .with_target(false)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_filter(filter_unwanted_crates(server_env_filter));

    let history = HISTORY.clone();
    MANAGER.lock().unwrap().process = Some(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(message) => {
                    history.lock().unwrap().push(message);
                }
                Err(_) => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
            }
        }
    }));

    // Configure the default subscriber
    match cli::manager::is_tracy() {
        true => {
            let tracy_layer = tracing_tracy::TracyLayer::default();
            let subscriber = tracing_subscriber::registry()
                .with(console_layer)
                .with(file_layer)
                .with(server_layer)
                .with(tracy_layer);
            tracing::subscriber::set_global_default(subscriber)
                .expect("Unable to set a global subscriber");
        }
        false => {
            let subscriber = tracing_subscriber::registry()
                .with(console_layer)
                .with(file_layer)
                .with(server_layer);
            tracing::subscriber::set_global_default(subscriber)
                .expect("Unable to set a global subscriber");
        }
    };

    // Configure GSTreamer logs integration
    gst::log::remove_default_log_function();
    if cli::manager::is_tracy() {
        tracing_gstreamer::integrate_events(); // This must be called before any gst::init()
        gst::init().unwrap();
        tracing_gstreamer::integrate_spans(); // This must be called after gst::init(), this is necessary to have GStreamer on tracy
    } else {
        redirect_gstreamer_logs_to_tracing();
    }

    info!(
        "{}, version: {}-{}, build date: {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        env!("VERGEN_GIT_SHA_SHORT"),
        env!("VERGEN_BUILD_DATE")
    );
    info!(
        "Starting at {}",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
    );
    info!("Server running at {}", cli::manager::server_address(),);
    debug!("Command line call: {}", cli::manager::command_line_string());
    debug!(
        "Command line input struct call: {}",
        cli::manager::command_line()
    );
}

fn redirect_gstreamer_logs_to_tracing() {
    gst::log::add_log_function(
        |category, gst_level, file, function, line, object, message| {
            use gst::DebugLevel as GstLevel;
            use tracing::Level as TracingLevel;

            let tracing_level: TracingLevel = match gst_level {
                GstLevel::Log | GstLevel::Info | GstLevel::None => TracingLevel::INFO,
                GstLevel::Debug => TracingLevel::DEBUG,
                GstLevel::Warning | GstLevel::Fixme => TracingLevel::WARN,
                GstLevel::Error => TracingLevel::ERROR,
                _ => TracingLevel::TRACE,
            };

            let object_name = object.map(|o| format!(":<{}>", o)).unwrap_or_default();
            let category_name = category.name();
            let message = message.get().map(|o| o.to_string()).unwrap_or_default();

            // Quick workaround to use dynamic level, because the macro requires it to be static
            macro_rules! dyn_event {
                (target: $target:expr, parent: $parent:expr, $lvl:ident, $($fields:tt)*) => {
                    match $lvl {
                        TracingLevel::INFO => ::tracing::info!(target: $target, parent: $parent, $($fields)*),
                        TracingLevel::DEBUG => ::tracing::debug!(target: $target, parent: $parent, $($fields)*),
                        TracingLevel::WARN => ::tracing::warn!(target: $target, parent: $parent, $($fields)*),
                        TracingLevel::ERROR => ::tracing::error!(target: $target, parent: $parent, $($fields)*),
                        TracingLevel::TRACE => ::tracing::trace!(target: $target, parent: $parent, $($fields)*),
                    }
                };
            }

            dyn_event!(
                target: "gstreamer",
                parent: None,
                tracing_level,
                "{category_name} {file}:{line}:{function}{object_name} {message}",
            );
        },
    );
}

fn filter_unwanted_crates(env_filter: EnvFilter) -> EnvFilter {
    env_filter
        // Hyper is used for http request by our thread leak test
        // And it's pretty verbose when it's on
        .add_directive("hyper=off".parse().unwrap())
        // Reducing onvif-related libs verbosity
        .add_directive("yaserde=off".parse().unwrap())
        .add_directive("reqwest=off".parse().unwrap())
        .add_directive("onvif=off".parse().unwrap())
        .add_directive("schema=off".parse().unwrap())
        .add_directive("transport=off".parse().unwrap())
        .add_directive("validate=off".parse().unwrap())
        .add_directive("common=off".parse().unwrap())
        .add_directive("metadatastream=off".parse().unwrap())
        .add_directive("onvif_xsd=off".parse().unwrap())
        .add_directive("radiometry=off".parse().unwrap())
        .add_directive("rules=off".parse().unwrap())
        .add_directive("soap_envelope=off".parse().unwrap())
        .add_directive("types=off".parse().unwrap())
        .add_directive("xmlmime=off".parse().unwrap())
        .add_directive("xop=off".parse().unwrap())
        .add_directive("accesscontrol=off".parse().unwrap())
        .add_directive("accessrules=off".parse().unwrap())
        .add_directive("actionengine=off".parse().unwrap())
        .add_directive("advancedsecurity=off".parse().unwrap())
        .add_directive("analytics=off".parse().unwrap())
        .add_directive("authenticationbehavior=off".parse().unwrap())
        .add_directive("b_2=off".parse().unwrap())
        .add_directive("bf_2=off".parse().unwrap())
        .add_directive("credential=off".parse().unwrap())
        .add_directive("deviceio=off".parse().unwrap())
        .add_directive("devicemgmt=off".parse().unwrap())
        .add_directive("display=off".parse().unwrap())
        .add_directive("doorcontrol=off".parse().unwrap())
        .add_directive("event=off".parse().unwrap())
        .add_directive("imaging=off".parse().unwrap())
        .add_directive("media=off".parse().unwrap())
        .add_directive("media2=off".parse().unwrap())
        .add_directive("provisioning=off".parse().unwrap())
        .add_directive("ptz=off".parse().unwrap())
        .add_directive("receiver=off".parse().unwrap())
        .add_directive("recording=off".parse().unwrap())
        .add_directive("replay=off".parse().unwrap())
        .add_directive("schedule=off".parse().unwrap())
        .add_directive("search=off".parse().unwrap())
        .add_directive("t_1=off".parse().unwrap())
        .add_directive("thermal=off".parse().unwrap())
        .add_directive("uplink=off".parse().unwrap())
        .add_directive("ws_addr=off".parse().unwrap())
        .add_directive("ws_discovery=off".parse().unwrap())
        .add_directive("xml_xsd=off".parse().unwrap())
        .add_directive("onvif::discovery=off".parse().unwrap())
}
