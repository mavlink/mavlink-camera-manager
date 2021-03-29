#[macro_use]
extern crate lazy_static;
extern crate log;
extern crate simple_error;
extern crate sys_info;

mod cli;
mod custom;
mod logger;
mod master;
mod mavlink;
mod network;
mod server;
mod settings;
mod stream;
mod video;
mod video_stream;

use log::*;

/**
 * Start our managers
 */
pub fn let_there_be_light() {
    // CLI should be started before logger to allow control over verbosity
    cli::manager::init();
    // Logger should start before everything else to register any log information
    logger::manager::init();
    // Settings should start before everybody else to ensure that the CLI are stored
    settings::manager::init(None);

    stream::manager::init();
    settings::manager::set_mavlink_endpoint(cli::manager::mavlink_connection_string());
    server::manager::run(cli::manager::server_address());
}

fn main() {
    let_there_be_light();

    master::run();

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
