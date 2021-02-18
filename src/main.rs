#[macro_use]
extern crate lazy_static;
extern crate simple_error;

mod cli;
mod mavlink;
mod server;
mod settings;
mod stream;
mod video;

/**
 * Start our managers
 */
pub fn let_there_be_light() {
    cli::manager::init();
    settings::manager::init("/tmp/potato.toml");
    stream::manager::init();
    server::manager::run(cli::manager::server_address());
}

fn main() {
    let_there_be_light();

    master::run();

    stream::manager::start(); //TODO: unify start and run

    println!("hello!");
    let l = mavlink::mavlink_camera::MavlinkCameraHandle::new();
    println!("verbose: {}", cli::manager::is_verbose());
    println!("created!");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
