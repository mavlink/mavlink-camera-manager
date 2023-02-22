use clap;
use std::sync::Arc;
use tracing::error;

use crate::{custom, stream::gst::utils::PluginRankConfig};

#[derive(Debug)]
struct Manager<'a> {
    clap_matches: clap::ArgMatches<'a>,
}

lazy_static! {
    static ref MANAGER: Arc<Manager<'static>> = Arc::new(Manager::new());
    static ref CURRENT_EXECUTION_WWW_PATH: String = format!(
        "{}/www",
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
    );
}

impl Manager<'_> {
    fn new() -> Self {
        Self {
            clap_matches: get_clap_matches(),
        }
    }
}

// Construct our manager, should be done inside main
pub fn init() {
    MANAGER.as_ref();
}

// Check if the verbosity parameter was used
pub fn is_verbose() -> bool {
    return MANAGER.as_ref().clap_matches.is_present("verbose");
}

pub fn is_tracing() -> bool {
    return MANAGER
        .as_ref()
        .clap_matches
        .is_present("enable-tracing-level-log-file");
}

pub fn is_reset() -> bool {
    return MANAGER.as_ref().clap_matches.is_present("reset");
}

#[allow(dead_code)]
// Return the mavlink connection string
pub fn mavlink_connection_string() -> Option<&'static str> {
    return MANAGER.as_ref().clap_matches.value_of("mavlink");
}

pub fn log_path() -> String {
    MANAGER
        .as_ref()
        .clap_matches
        .value_of("log-path")
        .expect("Clap arg \"log-path\" should always be \"Some(_)\" because of the default value.")
        .to_string()
}

// Return the desired address for the REST API
pub fn server_address() -> &'static str {
    return MANAGER
        .as_ref()
        .clap_matches
        .value_of("rest-server")
        .unwrap();
}

pub fn vehicle_ddns() -> Option<&'static str> {
    MANAGER.as_ref().clap_matches.value_of("vehicle-ddns")
}

pub fn default_settings() -> Option<&'static str> {
    return MANAGER.as_ref().clap_matches.value_of("default-settings");
}

// Return the command line used to start this application
pub fn command_line_string() -> String {
    std::env::args().collect::<Vec<String>>().join(" ")
}

// Return clap::ArgMatches struct
pub fn matches<'a>() -> clap::ArgMatches<'a> {
    return MANAGER.as_ref().clap_matches.clone();
}

pub fn gst_feature_rank() -> Vec<PluginRankConfig> {
    let values = MANAGER
        .clap_matches
        .values_of("gst-feature-rank")
        .unwrap_or_default()
        .collect::<Vec<&str>>();
    values
        .iter()
        .filter_map(|&val| {
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
                    rank: gst::Rank::__Unknown(value),
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

fn get_clap_matches<'a>() -> clap::ArgMatches<'a> {
    let version = format!(
        "{}-{} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("VERGEN_GIT_SHA_SHORT"),
        env!("VERGEN_BUILD_DATE")
    );

    let matches = clap::App::new(env!("CARGO_PKG_NAME"))
        .version(version.as_str())
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .arg(
            clap::Arg::with_name("mavlink")
                .long("mavlink")
                .value_name("TYPE>:<IP/SERIAL>:<PORT/BAUDRATE")
                .help("Sets the mavlink connection string")
                .takes_value(true)
        )
        .arg(
            clap::Arg::with_name("default-settings")
                .long("default-settings")
                .value_name("NAME")
                .possible_values(&custom::CustomEnvironment::variants())
                .help("Default settings to be used for different vehicles or environments.")
                .takes_value(true)
        )
        .arg(
            clap::Arg::with_name("reset")
                .long("reset")
                .help("Deletes settings file before starting.")
                .takes_value(false),
        )
        .arg(
            clap::Arg::with_name("rest-server")
                .long("rest-server")
                .value_name("IP>:<PORT")
                .help("Sets the address for the REST API server")
                .takes_value(true)
                .default_value("0.0.0.0:6020"),
        )
        .arg(
            clap::Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Turns all log categories up to Debug, for more information check RUST_LOG env variable.")
                .takes_value(false),
        )
        .arg(
            clap::Arg::with_name("gst-feature-rank")
            .long("gst-feature-rank")
            .help("Sets the Rank for the given Gst features. GST_PLUGIN_NAME is a string, and GST_RANK_INT_VALUE a valid 32 bits signed integer. A comma-separated list is also accepted. Example: \"omxh264enc=264,v4l2h264enc=0,x264enc=263\" (without quotes)")
            .value_name("GST_PLUGIN_NAME>=<GST_RANK_INT_VALUE")
            .value_delimiter(",")
            .multiple(true)
            .empty_values(false)
            .case_insensitive(true)
            .validator(gst_feature_rank_validator)
        )
        .arg(
            clap::Arg::with_name("log-path")
                .long("log-path")
                .help("Specifies the path in witch the logs will be stored.")
                .default_value("./logs")
                .takes_value(true),
        )
        .arg(
            clap::Arg::with_name("enable-tracing-level-log-file")
                .long("enable-tracing-level-log-file")
                .help("Turns all log categories up to Trace to the log file, for more information check RUST_LOG env variable.")
                .takes_value(false),
        )
        .arg(
            clap::Arg::with_name("vehicle-ddns")
                .long("vehicle-ddns")
                .help("Specifies the Dynamic DNS to use as vehicle IP when advertising streams via mavlink.")
                .takes_value(true),
        );

    matches.get_matches()
}

fn gst_feature_rank_validator(val: String) -> Result<(), String> {
    if let Some((_key, value_str)) = val.split_once('=') {
        if value_str.parse::<i32>().is_err() {
            return Err("GST_RANK_INT_VALUE should be a valid 32 bits signed integer, like \"-1\", \"0\" or \"256\" (without quotes).".to_string());
        }
    } else {
        return Err("Unexpected format, it should be <GST_PLUGIN_NAME>=<GST_RANK_INT_VALUE>, where GST_PLUGIN_NAME is a string, and GST_RANK_INT_VALUE a valid 32 bits signed integer. Example: \"omxh264enc=264\" (without quotes).".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_arguments() {
        assert_eq!(is_verbose(), false);
    }
}
