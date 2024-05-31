use anyhow::Context;
use clap;
use std::sync::Arc;
use tracing::error;

use crate::{custom, stream::gst::utils::PluginRankConfig};

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"), about = env!("CARGO_PKG_DESCRIPTION"))]
struct Args {
    /// Sets the mavlink connection string
    #[arg(
        long,
        value_name = "TYPE>:<IP/SERIAL>:<PORT/BAUDRATE",
        default_value = "udpin:0.0.0.0:14550"
    )]
    mavlink: String,

    /// Sets the settings file path
    #[arg(
        long,
        value_name = "./settings.json",
        default_value = "~/.config/mavlink-camera-manager/settings.json"
    )]
    settings_file: String,

    /// Default settings to be used for different vehicles or environments.
    #[arg(long, value_name = "NAME")]
    default_settings: Option<custom::CustomEnvironment>,

    /// Deletes settings file before starting.
    #[arg(long)]
    reset: bool,

    /// Sets the address for the REST API server
    #[arg(long, value_name = "IP>:<PORT", default_value = "0.0.0.0:6020")]
    rest_server: String,

    /// Sets the address for the stun server
    #[arg(
        long,
        value_name = "stun://IP>:<PORT",
        default_value = "stun://0.0.0.0:3478"
    )]
    stun_server: String,

    /// Sets the address for the Signalling server API server
    #[arg(
        long,
        value_name = "ws://IP>:<PORT",
        default_value = "ws://0.0.0.0:6021"
    )]
    signalling_server: String,

    /// Turns all log categories up to Debug, for more information check RUST_LOG env variable.
    #[arg(short, long)]
    verbose: bool,

    /// Sets the Rank for the given Gst features.
    #[clap(long, value_name = "GST_PLUGIN_NAME>=<GST_RANK_INT_VALUE", value_delimiter = ',', value_parser = gst_feature_rank_validator)]
    gst_feature_rank: Vec<String>,

    /// Specifies the path in witch the logs will be stored.
    #[arg(long, default_value = "./logs")]
    log_path: Option<String>,

    /// Turns all log categories up to Trace to the log file, for more information check RUST_LOG env variable.
    #[arg(long)]
    enable_tracing_level_log_file: bool,

    /// Specifies the Dynamic DNS to use as vehicle IP when advertising streams via mavlink.
    #[arg(long)]
    vehicle_ddns: Option<String>,

    /// Turns on the Tracy tool integration.
    #[arg(long)]
    enable_tracy: bool,

    /// Enable a thread that prints the number of children processes.
    #[arg(long)]
    enable_thread_counter: bool,

    /// Enable webrtc thread test with limit of child tasks (can use port for webdriver as parameter).
    #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "9515")]
    enable_webrtc_task_test: Option<u16>,

    /// Sets the MAVLink System ID.
    #[arg(long, value_name = "SYSTEM_ID", default_value = "1")]
    mavlink_system_id: u8,
}

#[derive(Debug)]
struct Manager {
    clap_matches: Args,
}

lazy_static! {
    static ref MANAGER: Arc<Manager> = Arc::new(Manager::new());
    static ref CURRENT_EXECUTION_WWW_PATH: String = format!(
        "{}/www",
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(anyhow::Error::msg)
            .and_then(|path| path
                .to_str()
                .context("Failed to convert path to str")
                .map(String::from))
            .expect("Failed to get current executable path")
    );
}

impl Manager {
    fn new() -> Self {
        Self {
            clap_matches: Args::parse(),
        }
    }
}

// Construct our manager, should be done inside main
pub fn init() {
    MANAGER.as_ref();
}

// Check if the verbosity parameter was used
pub fn is_verbose() -> bool {
    MANAGER.clap_matches.verbose
}

pub fn is_tracing() -> bool {
    MANAGER.clap_matches.enable_tracing_level_log_file
}

pub fn is_reset() -> bool {
    MANAGER.clap_matches.reset
}

pub fn is_tracy() -> bool {
    MANAGER.clap_matches.enable_tracy
}

#[allow(dead_code)]
// Return the mavlink connection string
pub fn mavlink_connection_string() -> String {
    MANAGER.clap_matches.mavlink.clone()
}

pub fn log_path() -> String {
    MANAGER
        .clap_matches
        .log_path
        .clone()
        .expect("Clap arg \"log-path\" should always be \"Some(_)\" because of the default value.")
}

// Return the desired settings file
pub fn settings_file() -> String {
    MANAGER.clap_matches.settings_file.clone()
}

// Return the desired address for the REST API
pub fn server_address() -> String {
    MANAGER.clap_matches.rest_server.clone()
}

// Return the desired address for the STUN server
pub fn stun_server_address() -> String {
    MANAGER.clap_matches.stun_server.clone()
}

// Return the desired address for the signalling server
pub fn signalling_server_address() -> String {
    MANAGER.clap_matches.signalling_server.clone()
}

pub fn vehicle_ddns() -> Option<String> {
    MANAGER.clap_matches.vehicle_ddns.clone()
}

pub fn default_settings() -> Option<custom::CustomEnvironment> {
    MANAGER.clap_matches.default_settings.clone()
}

pub fn enable_thread_counter() -> bool {
    MANAGER.clap_matches.enable_thread_counter
}

pub fn enable_webrtc_task_test() -> Option<u16> {
    MANAGER.clap_matches.enable_webrtc_task_test
}

pub fn mavlink_system_id() -> u8 {
    MANAGER.clap_matches.mavlink_system_id
}

// Return the command line used to start this application
pub fn command_line_string() -> String {
    std::env::args().collect::<Vec<String>>().join(" ")
}

// Return a clone of current Args struct
pub fn command_line() -> String {
    format!("{:#?}", MANAGER.clap_matches)
}

pub fn gst_feature_rank() -> Vec<PluginRankConfig> {
    MANAGER.clap_matches.gst_feature_rank
        .iter()
        .filter_map(|val| {
            if let Some((key, value_str)) = val.split_once('=') {
                let value = match value_str.parse::<i32>() {
                    Ok(value) => value,
                    Err(error) => {
                        error!(
                            "Failed parsing {value_str:?} to i32, ignoring feature rank {key:?}. Reason: {error:#?}"
                        );
                        return None;
                    }
                };

                let config = PluginRankConfig {
                    name: key.to_string(),
                    rank: gst::Rank::from(value),
                };
                return Some(config);
            }
            error!(
                "Failed parsing {val:?} to <str>=<i32>, ignoring this feature rank."
            );
            None
        })
        .collect()
}

fn gst_feature_rank_validator(val: &str) -> Result<String, String> {
    if let Some((_key, value_str)) = val.split_once('=') {
        if value_str.parse::<i32>().is_err() {
            return Err("GST_RANK_INT_VALUE should be a valid 32 bits signed integer, like \"-1\", \"0\" or \"256\" (without quotes).".to_string());
        }
    } else {
        return Err("Unexpected format, it should be <GST_PLUGIN_NAME>=<GST_RANK_INT_VALUE>, where GST_PLUGIN_NAME is a string, and GST_RANK_INT_VALUE a valid 32 bits signed integer. Example: \"omxh264enc=264\" (without quotes).".to_string());
    }
    Ok(val.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_arguments() {
        assert!(!is_verbose());
    }
}
