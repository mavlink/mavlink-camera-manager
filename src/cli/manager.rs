use clap;
use std::sync::Arc;

use crate::custom;

#[derive(Debug)]
struct Manager<'a> {
    clap_matches: clap::ArgMatches<'a>,
}

lazy_static! {
    static ref MANAGER: Arc<Manager<'static>> = Arc::new(Manager::new());
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

pub fn is_reset() -> bool {
    return MANAGER.as_ref().clap_matches.is_present("reset");
}

#[allow(dead_code)]
// Return the mavlink connection string
pub fn mavlink_connection_string() -> &'static str {
    return MANAGER.as_ref().clap_matches.value_of("mavlink").unwrap();
}

// Return the desired address for the REST API
pub fn server_address() -> &'static str {
    return MANAGER
        .as_ref()
        .clap_matches
        .value_of("rest-server")
        .unwrap();
}

pub fn default_settings() -> Option<&'static str> {
    return MANAGER.as_ref().clap_matches.value_of("default-settings");
}

// Return the command line used to start this application
pub fn command_line_string() -> String {
    return std::env::args().collect::<Vec<String>>().join(" ");
}

// Return clap::ArgMatches struct
pub fn matches<'a>() -> clap::ArgMatches<'a> {
    return MANAGER.as_ref().clap_matches.clone();
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
                .value_name("TYPE:<IP/SERIAL>:<PORT/BAUDRATE>")
                .help("Sets the mavlink connection string")
                .takes_value(true)
                .default_value("udpout:0.0.0.0:14550"),
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
                .help("Delete settings file before starting.")
                .takes_value(false),
        )
        .arg(
            clap::Arg::with_name("rest-server")
                .long("rest-server")
                .help("Sets the address for the REST API server")
                .takes_value(true)
                .default_value("0.0.0.0:6020"),
        )
        .arg(
            clap::Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Turn all log categories up to Debug, for more information check RUST_LOG env variable.")
                .takes_value(false),
        );

    return matches.get_matches();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_arguments() {
        assert_eq!(is_verbose(), false);
        assert_eq!(mavlink_connection_string(), "udpout:0.0.0.0:14550");
    }
}
