#[macro_use]
extern crate lazy_static;
extern crate log;
extern crate simple_error;

mod cli;
mod mavlink;
mod server;
mod settings;
mod stream;
mod video;

mod logger;
mod master;
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

    settings::manager::init("/tmp/potato.toml");
    stream::manager::init();
    server::manager::run(cli::manager::server_address());
}

fn main() {
    let_there_be_light();

    master::run();

    stream::manager::start(); //TODO: unify start and run
    let l = mavlink::mavlink_camera::MavlinkCameraHandle::new();
    info!("verbose: {}", cli::manager::is_verbose());
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
