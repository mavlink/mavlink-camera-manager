#[macro_use]
extern crate lazy_static;

mod manager;

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
    return;
}
