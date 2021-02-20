use clap;
use log::*;
use std::sync::Arc;

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

pub fn init() {
    MANAGER.as_ref();
}

pub fn is_verbose() -> bool {
    return MANAGER.as_ref().clap_matches.is_present("verbose");
}

#[allow(dead_code)]
pub fn mavlink_connection_string() -> &'static str {
    return MANAGER.as_ref().clap_matches.value_of("mavlink").unwrap();
}

pub fn server_address() -> &'static str {
    return MANAGER
        .as_ref()
        .clap_matches
        .value_of("rest-server")
        .unwrap();
}

pub fn command_line_string() -> String {
    return std::env::args().collect::<Vec<String>>().join(" ");
}

pub fn matches<'a>() -> clap::ArgMatches<'a> {
    return MANAGER.as_ref().clap_matches.clone();
}

pub fn get_clap_matches<'a>() -> clap::ArgMatches<'a> {
    let matches = clap::App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
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
