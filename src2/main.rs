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
    stream::video_stream_manager::init();
}

fn main() {
    let_there_be_light();
    println!("hello!");
    println!("verbose: {}", manager::command_line::is_verbose());
    loop {
        println!("created!");
        std::thread::sleep(std::time::Duration::from_millis(2000));
    }
}
