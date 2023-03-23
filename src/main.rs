#[macro_use]
extern crate lazy_static;
extern crate paperclip;
extern crate sys_info;
extern crate tracing;

#[macro_use]
mod helper;

use tracing::*;
mod cli;
mod custom;
mod logger;
mod mavlink;
mod network;
mod server;
mod settings;
mod stream;
mod video;
mod video_stream;

#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    // CLI should be started before logger to allow control over verbosity
    cli::manager::init();
    // Logger should start before everything else to register any log information
    logger::manager::init();
    // Settings should start before everybody else to ensure that the CLI are stored
    settings::manager::init(None);

    stream::manager::init();
    if let Some(endpoint) = cli::manager::mavlink_connection_string() {
        settings::manager::set_mavlink_endpoint(endpoint);
    }

    stream::webrtc::signalling_server::SignallingServer::default();

    if let Err(error) = stream::manager::start_default() {
        error!("Failed to start default streams. Reason: {error:?}")
    }

    server::manager::run(cli::manager::server_address()).await
}
