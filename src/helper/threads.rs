use std::thread;
use std::time::Duration;
use sysinfo::{System, SystemExt};
use tracing::*;

pub fn start_thread_counter_thread() {
    let mut system = System::new_all();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    thread::spawn(move || loop {
        system.refresh_process(pid);
        info!(
            "Number of child processes: {}",
            system.process(pid).unwrap().tasks.len()
        );
        thread::sleep(Duration::from_secs(1));
    });
}
