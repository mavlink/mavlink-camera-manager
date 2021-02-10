#[macro_use]
extern crate lazy_static;
extern crate simple_error;

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
    loop {
        let mut udp_stream = stream::video_stream_udp::VideoStreamUdp::default();
        println!("start stream!");
        udp_stream.start();
        std::thread::sleep(std::time::Duration::from_millis(3000));
        println!("stop stream!");
        udp_stream.stop();
        std::thread::sleep(std::time::Duration::from_millis(3000));
        println!("drop it!");
        drop(udp_stream);
        println!("bye bye");
        std::thread::sleep(std::time::Duration::from_millis(3000));
    }
}
