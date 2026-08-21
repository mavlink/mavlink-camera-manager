use std::{collections::HashMap, ffi::OsString, sync::Arc, time::Duration};

use anyhow::Context;
use clap;
use tracing::error;

use crate::{custom, stream::gst::utils::PluginRankConfig};

use clap::{Parser, ValueEnum};
use constcat::concat;

#[derive(Parser, Debug, Clone)]
#[command(
    version = env!("CARGO_PKG_VERSION"),
    author = env!("CARGO_PKG_AUTHORS"),
    about = env!("CARGO_PKG_DESCRIPTION"),
    after_help = "Every argv element is expanded ($NAME, ${NAME}, ${NAME:-default}; $$ for a literal $). An unset var leaves the whole element as-is. MCM_ONVIF_AUTH and MCM_TURN_SERVERS are used as-is.",
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

    /// Sets the addresses for the turn servers. Alternatively, this can be passed as `MCM_TURN_SERVERS` environment variable.
    #[arg(
        long,
        value_name = "turn(s)://[<USERNAME>:<PASSWORD>@]<HOST>:<PORT>",
        value_delimiter = ',',
        env = "MCM_TURN_SERVERS",
        hide_env_values = true
    )]
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

    /// Enable the WebRTC task test and optionally choose the WebDriver port.
    #[arg(long, value_name = "PORT", default_value_t = 9515)]
    enable_webrtc_task_test: u32,

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
    #[clap(
        long,
        value_name = "onvif://<USERNAME>:<PASSWORD>@<HOST>",
        value_delimiter = ',',
        env = "MCM_ONVIF_AUTH",
        hide_env_values = true
    )]
    onvif_auth: Vec<String>,

    /// Enable the /dot WebSocket endpoint for GStreamer pipeline graph streaming.
    #[arg(long)]
    enable_dot: bool,

    /// Enables the zenoh integration by default in client mode.
    #[arg(long)]
    zenoh: bool,

    /// Sets the zenoh configuration file path.
    #[arg(long, value_name = "PATH")]
    zenoh_config_file: Option<String>,

    /// Enable real-time (SCHED_RR) thread scheduling for GStreamer pipeline
    /// threads. Requires CAP_SYS_NICE. When disabled (default), pipeline
    /// threads run under normal SCHED_OTHER scheduling.
    #[arg(long)]
    enable_realtime_threads: bool,

    /// Sets the RTSP server listen port.
    #[arg(long, value_name = "PORT", default_value_t = 8554)]
    rtsp_port: u16,

    /// Disable ONVIF camera discovery.
    #[arg(long)]
    disable_onvif: bool,

    /// How long to keep retrying failed stream recreation before removing the
    /// stream. Use `none` to retry forever, or `0` to remove immediately.
    #[arg(
        long,
        value_name = "SECONDS|none",
        default_value = "300",
        value_parser = stream_recreation_failure_timeout_validator
    )]
    stream_recreation_failure_timeout: StreamRecreationFailureTimeoutArg,

    /// Video recording backend. With `external`, recording capability is
    /// advertised via MAVLink but handled by an external service (e.g. BlueOS
    /// Recorder).
    #[arg(long, value_name = "external")]
    recorder: Option<RecorderMode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RecorderMode {
    External,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StreamRecreationFailureTimeoutArg {
    Never,
    Seconds(u64),
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
        if cfg!(test) {
            return Self {
                clap_matches: Args::parse_from(["mavlink-camera-manager"]),
            };
        }

        let (expanded_args, expand_errors) = expand_args(std::env::args_os());
        // Keep the raw arg on failure, same as blueos-recorder.
        for (index, error) in &expand_errors {
            eprintln!(
                "Failed expanding argv index {index}, using the non-expanded instead: {error}"
            );
        }

        let clap_matches = Args::parse_from(expanded_args);
        // Clap echoes the value on a value_parser error, so checks live here.
        reject_invalid_urls(&clap_matches.turn_servers, turn_url_error);
        reject_invalid_urls(&clap_matches.onvif_auth, onvif_url_error);
        Self { clap_matches }
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
    expand_tilde(
        MANAGER.clap_matches.log_path.as_deref().expect(
            "Clap arg \"log-path\" should always be \"Some(_)\" because of the default value.",
        ),
    )
}

// Return the desired settings file
pub fn settings_file() -> String {
    expand_tilde(&MANAGER.clap_matches.settings_file)
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

pub fn enable_webrtc_task_test() -> Option<u32> {
    Some(MANAGER.clap_matches.enable_webrtc_task_test)
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

// Debug dump of the parsed CLI. Passwords in this dump are redacted.
pub fn command_line() -> String {
    format!("{:#?}", redacted(&MANAGER.clap_matches))
}

fn redacted(args: &Args) -> Args {
    let mut args = args.clone();
    args.turn_servers = args
        .turn_servers
        .iter()
        .map(|value| redact_url_password(value))
        .collect();
    args.onvif_auth = args
        .onvif_auth
        .iter()
        .map(|value| redact_url_password(value))
        .collect();
    args
}

fn reject_invalid_urls(values: &[String], error_for: fn(&str) -> Option<String>) {
    for (index, value) in values.iter().enumerate() {
        if let Some(error) = error_for(value) {
            eprintln!("{index}: {}: {error}", redact_url_password(value));
            std::process::exit(2);
        }
    }
}

fn turn_url_error(value: &str) -> Option<String> {
    let url = match url::Url::parse(value) {
        Ok(url) => url,
        Err(error) => return Some(format!("Failed parsing turn url: {error:?}")),
    };
    if !matches!(url.scheme().to_lowercase().as_str(), "turn" | "turns") {
        return Some("Turn server scheme should be either \"turn\" or \"turns\"".to_owned());
    }
    if url.host_str().is_none() {
        return Some("Turn server url should include a host".to_owned());
    }
    None
}

fn onvif_url_error(value: &str) -> Option<String> {
    let url = match url::Url::parse(value) {
        Ok(url) => url,
        Err(error) => return Some(format!("Failed parsing onvif auth url: {error:?}")),
    };
    if !matches!(url.scheme().to_lowercase().as_str(), "onvif") {
        return Some("Onvif authentication scheme should be \"onvif\"".to_owned());
    }
    if url.host_str().is_none() {
        return Some("Onvif authentication url should include a host".to_owned());
    }
    None
}

fn redact_url_password(value: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(value) else {
        return "***".to_owned();
    };
    if parsed.cannot_be_a_base() {
        return format!("{}:***", parsed.scheme());
    }
    if parsed.password().is_none() {
        return value.to_owned();
    }
    if parsed.set_password(Some("***")).is_err() {
        return "***".to_owned();
    }
    parsed.to_string()
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
                        error!(
                            "Failed to get credentials from url {}: {error:?}",
                            redact_url_password(url.as_str())
                        );
                        return None;
                    }
                };

            Some((host, credentials))
        })
        .collect()
}

pub fn is_dot_enabled() -> bool {
    MANAGER.clap_matches.enable_dot
}

pub fn enable_zenoh() -> bool {
    MANAGER.clap_matches.zenoh
}

pub fn zenoh_config_file() -> Option<String> {
    MANAGER
        .clap_matches
        .zenoh_config_file
        .as_ref()
        .map(|path| expand_tilde(path))
}

pub fn enable_realtime_threads() -> bool {
    MANAGER.clap_matches.enable_realtime_threads
}

pub fn rtsp_server_port() -> u16 {
    MANAGER.clap_matches.rtsp_port
}

pub fn is_onvif_disabled() -> bool {
    MANAGER.clap_matches.disable_onvif
}

pub fn stream_recreation_failure_timeout() -> Option<Duration> {
    match MANAGER.clap_matches.stream_recreation_failure_timeout {
        StreamRecreationFailureTimeoutArg::Never => None,
        StreamRecreationFailureTimeoutArg::Seconds(secs) => Some(Duration::from_secs(secs)),
    }
}

pub fn recorder_mode() -> Option<RecorderMode> {
    MANAGER.clap_matches.recorder
}

fn expand_args(args: impl Iterator<Item = OsString>) -> (Vec<OsString>, Vec<(usize, String)>) {
    let mut errors = Vec::new();
    let expanded = args
        .enumerate()
        .map(|(index, arg)| match arg.into_string() {
            Ok(arg) => match shellexpand::env(&arg) {
                Ok(expanded) => expanded.into_owned().into(),
                Err(error) => {
                    errors.push((
                        index,
                        match error.cause {
                            std::env::VarError::NotPresent => "variable not set".into(),
                            std::env::VarError::NotUnicode(_) => {
                                "variable value is not valid utf-8".into()
                            }
                        },
                    ));
                    arg.into()
                }
            },
            Err(arg) => {
                errors.push((index, "not valid utf-8".into()));
                arg
            }
        })
        .collect();
    (expanded, errors)
}

fn expand_tilde(value: &str) -> String {
    shellexpand::tilde(value).into_owned()
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

fn stream_recreation_failure_timeout_validator(
    val: &str,
) -> Result<StreamRecreationFailureTimeoutArg, String> {
    if val.eq_ignore_ascii_case("none") {
        return Ok(StreamRecreationFailureTimeoutArg::Never);
    }

    let secs = val
        .parse::<u64>()
        .map_err(|_| "Expected a non-negative integer number of seconds or \"none\"".to_string())?;
    Ok(StreamRecreationFailureTimeoutArg::Seconds(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_arguments() {
        assert!(!is_verbose());
        assert_eq!(enable_webrtc_task_test(), Some(9515));
        assert_eq!(
            stream_recreation_failure_timeout(),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn stream_recreation_failure_timeout_accepts_none() {
        let args = Args::parse_from([
            "mavlink-camera-manager",
            "--stream-recreation-failure-timeout",
            "none",
        ]);

        assert_eq!(
            args.stream_recreation_failure_timeout,
            StreamRecreationFailureTimeoutArg::Never
        );
    }

    #[test]
    fn stream_recreation_failure_timeout_accepts_zero() {
        let args = Args::parse_from([
            "mavlink-camera-manager",
            "--stream-recreation-failure-timeout",
            "0",
        ]);

        assert_eq!(
            args.stream_recreation_failure_timeout,
            StreamRecreationFailureTimeoutArg::Seconds(0)
        );
    }

    #[test]
    fn expand_args_keeps_plain() {
        let (expanded, errors) = expand_args(["mcm", "plain"].map(OsString::from).into_iter());
        assert_eq!(expanded, ["mcm", "plain"].map(OsString::from));
        assert!(errors.is_empty());
    }

    #[test]
    fn expand_args_substitutes_set_var() {
        let path = std::env::var("PATH").expect("PATH");
        let (expanded, errors) = expand_args(["mcm", "$PATH"].map(OsString::from).into_iter());
        assert_eq!(expanded, [OsString::from("mcm"), OsString::from(path)]);
        assert!(errors.is_empty());
    }

    #[test]
    fn expand_args_keeps_unset_var() {
        assert!(std::env::var("MCM_EXPAND_UNSET_VAR_XYZ").is_err());
        let (expanded, errors) = expand_args(
            ["mcm", "$MCM_EXPAND_UNSET_VAR_XYZ"]
                .map(OsString::from)
                .into_iter(),
        );
        assert_eq!(
            expanded,
            ["mcm", "$MCM_EXPAND_UNSET_VAR_XYZ"].map(OsString::from)
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, 1);
        assert!(!errors[0].1.contains("MCM_EXPAND_UNSET_VAR_XYZ"));
    }

    #[test]
    fn expand_tilde_keeps_plain_path() {
        assert_eq!(expand_tilde("plain/path"), "plain/path");
    }

    #[test]
    fn expand_tilde_expands_home() {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .expect("HOME");
        assert_eq!(expand_tilde("~/x"), format!("{home}/x"));
    }

    #[cfg(unix)]
    #[test]
    fn expand_args_keeps_non_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0x2d, 0x2d, 0xff, 0xfe]);
        let (expanded, errors) = expand_args([OsString::from("mcm"), bad.clone()].into_iter());
        assert_eq!(expanded, [OsString::from("mcm"), bad]);
        assert_eq!(errors, [(1, "not valid utf-8".into())]);
    }

    #[test]
    fn redact_url_password_hides_password() {
        assert_eq!(
            redact_url_password("turn://user:s3cretpw@host:3478"),
            "turn://user:***@host:3478"
        );
        assert_eq!(
            redact_url_password("turn:user:s3cretpw@host:3478"),
            "turn:***"
        );
        assert_eq!(redact_url_password("turn://host:3478"), "turn://host:3478");
    }

    #[test]
    fn command_line_dump_hides_passwords() {
        let args = Args::parse_from([
            "mavlink-camera-manager",
            "--turn-servers",
            "turn://user:s3cretpw@host:3478",
            "--onvif-auth",
            "onvif://user:s3cretpw@1.2.3.4",
        ]);
        let redacted_args = redacted(&args);
        assert_eq!(redacted_args.turn_servers, ["turn://user:***@host:3478"]);
        assert_eq!(redacted_args.onvif_auth, ["onvif://user:***@1.2.3.4"]);
        let dump = format!("{:#?}", redacted_args);
        assert!(!dump.contains("s3cretpw"));
    }

    #[test]
    fn turn_url_error_accepts_slashed_form() {
        assert_eq!(turn_url_error("turn://host:3478"), None);
        assert_eq!(turn_url_error("turns://user:s3cretpw@host:3478"), None);
    }

    #[test]
    fn turn_url_error_rejects_scheme_and_rfc7065() {
        assert_eq!(
            turn_url_error("http://user:s3cretpw@host:3478"),
            Some("Turn server scheme should be either \"turn\" or \"turns\"".into())
        );
        assert_eq!(
            turn_url_error("turn:host:3478"),
            Some("Turn server url should include a host".into())
        );
        assert!(turn_url_error("not a url").is_some());
    }

    #[test]
    fn onvif_url_error_accepts_and_rejects() {
        assert_eq!(onvif_url_error("onvif://user:s3cretpw@1.2.3.4"), None);
        assert_eq!(
            onvif_url_error("http://user:s3cretpw@1.2.3.4"),
            Some("Onvif authentication scheme should be \"onvif\"".into())
        );
        assert!(onvif_url_error("onvif://user:s3cretpw@").is_some());
    }
}
