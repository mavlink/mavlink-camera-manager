#[macro_use]
extern crate lazy_static;

#[macro_use]
extern crate simple_error;

mod manager;
mod stream;
mod video;

use stream::stream_backend::StreamBackend;

use std::thread;

/**
 * Start our managers
 */
pub fn let_there_be_light() {
    manager::command_line::init();
}

fn main() {
    let_there_be_light();

    let a = thread::spawn(move || {
        // some work here
    });

    println!("hello!");
    println!("verbose: {}", manager::command_line::is_verbose());

    {
        let mut udp_stream = stream::video_stream_udp::VideoStreamUdp::default();
        println!("video udp stream running at 0.0.0.0:5600");
        loop {
            udp_stream.start();
            std::thread::sleep(std::time::Duration::from_millis(3000));
            udp_stream.stop();
            std::thread::sleep(std::time::Duration::from_millis(3000));
            println!(".");
            break;
        }
    }
    println!("bye bye");
    std::thread::sleep(std::time::Duration::from_millis(30000));
}
