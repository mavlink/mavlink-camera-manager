#[macro_use]
extern crate lazy_static;
extern crate simple_error;

mod cli;
mod stream;
mod video;

use stream::stream_backend::StreamBackend;

/**
 * Start our managers
 */
pub fn let_there_be_light() {
    cli::manager::init();
    stream::manager::init();
}

fn main() {
    let_there_be_light();

    stream::manager::start();

    println!("hello!");
    println!("verbose: {}", cli::manager::is_verbose());
    loop {
        println!("created!");
        std::thread::sleep(std::time::Duration::from_millis(2000));
    }
}
