use cached::proc_macro::cached;
use std::thread;
use std::time::Duration;
use sysinfo::{System, SystemExt};
use tracing::*;

#[cached(time = 1)]
pub fn process_task_counter() -> usize {
    let mut system = System::new_all();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    system.refresh_process(pid);
    system.process(pid).unwrap().tasks.len()
}

pub fn start_thread_counter_thread() {
    thread::spawn(move || loop {
        info!("Number of child processes: {}", process_task_counter());
        thread::sleep(Duration::from_secs(1));
    });
}
