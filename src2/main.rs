#[macro_use]
extern crate lazy_static;
extern crate simple_error;

mod cli;
mod mavlink;
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
}

fn main() {
    let_there_be_light();

    stream::manager::start();

    println!("hello!");
    let l = mavlink::mavlink_camera::MavlinkCameraHandle::new();
    println!("verbose: {}", cli::manager::is_verbose());
    println!("created!");
    std::thread::sleep(std::time::Duration::from_millis(5000));
}
