#[macro_use]
extern crate lazy_static;

mod manager;
mod stream;
mod video;

use stream::stream_backend::StreamBackend;

/**
 * Start our managers
 */
pub fn let_there_be_light() {
    manager::command_line::init();
}

fn main() {
    let_there_be_light();

    println!("hello!");
    println!("verbose: {}", manager::command_line::is_verbose());

    let mut udp_stream = stream::video_stream_udp::VideoStreamUdp::default();
    udp_stream.start();
    println!("video udp stream running at 0.0.0.0:5600");
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        print!(".");
    }
}
