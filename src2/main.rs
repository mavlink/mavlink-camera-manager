#[macro_use]
extern crate lazy_static;
extern crate simple_error;

mod manager;
mod stream;
mod video;

use stream::stream_backend::StreamBackend;
use stream::video_stream_manager::VideoStreamManager;

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
        let mut stream_manager = stream::video_stream_manager::VideoStreamManager::default();
        println!("created!");
        std::thread::sleep(std::time::Duration::from_millis(3000));
        stream_manager.add();
        println!("added!");
        std::thread::sleep(std::time::Duration::from_millis(3000));
        stream_manager.start();
        println!("started!");
        std::thread::sleep(std::time::Duration::from_millis(3000));
        drop(stream_manager);
        println!("finished!");
        std::thread::sleep(std::time::Duration::from_millis(3000));
    }
}
