use anyhow::Context;
use clap;
use std::{collections::HashMap, sync::Arc};
use tracing::error;

use crate::{custom, stream::gst::utils::PluginRankConfig};

use clap::Parser;
use constcat::concat;

#[derive(Parser, Debug)]
#[command(
    version = env!("CARGO_PKG_VERSION"),
    author = env!("CARGO_PKG_AUTHORS"),
    about = env!("CARGO_PKG_DESCRIPTION"),
)]
struct Args {
    /// Sets the mavlink connection string
    #[arg(
        long,
        value_name = "<TYPE>:<IP/SERIAL>:<PORT/BAUDRATE>",
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
    #[arg(long, value_name = "<IP>:<PORT>", default_value = "0.0.0.0:6020")]
    rest_server: String,

    /// Sets the address for the stun server
    #[arg(
        long,
        value_name = "stun://<HOST>:<PORT>",
        default_value = "stun://0.0.0.0:3478"
    )]
    stun_server: String,

    /// Sets the addresses for the turn servers
    #[arg(long, value_name = "turn(s)://[<USERNAME>:<PASSWORD>@]<HOST>:<PORT>", value_delimiter = ',', value_parser = turn_servers_validator)]
    turn_servers: Vec<String>,

    /// Sets the address for the Signalling server API server
    #[arg(
        long,
        value_name = "ws://<IP>:<PORT>",
        default_value = "ws://0.0.0.0:6021"
    )]
    signalling_server: String,

    /// Turns all log categories up to Debug, for more information check RUST_LOG env variable.
    #[arg(short, long)]
    verbose: bool,

    /// Sets the Rank for the given Gst features.
    #[clap(long, value_name = "<GST_PLUGIN_NAME>=<GST_RANK_INT_VALUE>", value_delimiter = ',', value_parser = gst_feature_rank_validator)]
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

    /// Sets the MAVLink Component ID range to assign to cameras (e.g. 100-105).
    ///
    /// Note: 100–105 are reserved for autopilot-proxied cameras.
    /// QGroundControl expects cameras in that range, but 106+ is recommended.
    #[arg(
        long,
        value_name = "FIRST_ID-LAST_ID",
        default_value = "106-121",
        value_parser = mavlink_camera_component_id_range_validator
    )]
    mavlink_camera_component_id_range: std::ops::RangeInclusive<u8>,

    /// Sets Onvif authentications. Alternatively, this can be passed as `MCM_ONVIF_AUTH` environment variable.
    #[clap(long, value_name = "onvif://<USERNAME>:<PASSWORD>@<HOST>", value_delimiter = ',', value_parser = onvif_auth_validator, env = "MCM_ONVIF_AUTH")]
    onvif_auth: Vec<String>,

    /// Enables the zenoh integration by default in client mode.
    #[arg(long, value_name = "PATH")]
    zenoh: bool,

    /// Sets the zenoh configuration file path.
    #[arg(long, value_name = "PATH")]
    zenoh_config_file: Option<String>,
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
    let log_path =
        MANAGER.clap_matches.log_path.clone().expect(
            "Clap arg \"log-path\" should always be \"Some(_)\" because of the default value.",
        );

    shellexpand::full(&log_path)
        .expect("Failed to expand path")
        .to_string()
}

// Return the desired settings file
pub fn settings_file() -> String {
    let settings_file = MANAGER.clap_matches.settings_file.clone();

    shellexpand::full(&settings_file)
        .expect("Failed to expand path")
        .to_string()
}

// Return the desired address for the REST API
pub fn server_address() -> String {
    MANAGER.clap_matches.rest_server.clone()
}

// Return the desired address for the STUN server
pub fn stun_server_address() -> String {
    MANAGER.clap_matches.stun_server.clone()
}

// Return the desired address for the TURN server
pub fn turn_server_addresses() -> Vec<String> {
    MANAGER.clap_matches.turn_servers.clone()
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

pub fn mavlink_camera_component_id_range() -> std::ops::RangeInclusive<u8> {
    MANAGER
        .clap_matches
        .mavlink_camera_component_id_range
        .clone()
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

pub fn onvif_auth() -> HashMap<std::net::Ipv4Addr, onvif::soap::client::Credentials> {
    MANAGER
        .clap_matches
        .onvif_auth
        .iter()
        .filter_map(|val| {
            let url = match url::Url::parse(val) {
                Ok(url) => url,
                Err(error) => {
                    error!("Failed parsing onvif auth url: {error:?}");
                    return None;
                }
            };

            let (host, credentials) =
                match crate::controls::onvif::manager::Manager::credentials_from_url(&url) {
                    Ok((host, credentials)) => (host, credentials),
                    Err(error) => {
                        error!("Failed to get credentials from url {url}: {error:?}");
                        return None;
                    }
                };

            Some((host, credentials))
        })
        .collect()
}

pub fn enable_zenoh() -> bool {
    MANAGER.clap_matches.zenoh
}

pub fn zenoh_config_file() -> Option<String> {
    MANAGER.clap_matches.zenoh_config_file.clone()
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

fn turn_servers_validator(val: &str) -> Result<String, String> {
    let url = url::Url::parse(val).map_err(|e| format!("Failed parsing turn url: {e:?}"))?;

    if !matches!(url.scheme().to_lowercase().as_str(), "turn" | "turns") {
        return Err("Turn server scheme should be either \"turn\" or \"turns\"".to_owned());
    }

    Ok(val.to_owned())
}

fn onvif_auth_validator(val: &str) -> Result<String, String> {
    let url = url::Url::parse(val).map_err(|e| format!("Failed parsing onvif auth url: {e:?}"))?;

    if !matches!(url.scheme().to_lowercase().as_str(), "onvif") {
        return Err("Onvif authentication scheme should be \"onvif\"".to_owned());
    }

    Ok(val.to_owned())
}

fn mavlink_camera_component_id_range_validator(
    val: &str,
) -> Result<std::ops::RangeInclusive<u8>, String> {
    let parts: Vec<_> = val.split('-').collect();
    if parts.len() != 2 {
        return Err("Expected format: <first>-<last>".into());
    }

    let first_id = parts[0].parse::<u8>().map_err(|_| "Invalid first ID")?;
    let last_id = parts[1].parse::<u8>().map_err(|_| "Invalid last ID")?;

    if first_id > last_id {
        return Err("First ID must be smaller than the last ID".into());
    }

    Ok(first_id..=last_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_arguments() {
        assert!(!is_verbose());
    }
}
