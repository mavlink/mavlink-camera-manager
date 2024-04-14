use cached::proc_macro::cached;
use std::time::Duration;
use std::{collections::HashMap, thread};
use sysinfo::{PidExt, ProcessExt, System, SystemExt};
use tracing::*;

#[cached(time = 1)]
pub fn process_task_counter() -> usize {
    let mut system = System::new_all();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    system.refresh_process(pid);
    system.process(pid).unwrap().tasks.len()
}

#[cached(time = 1)]
pub fn process_tasks() -> HashMap<u32, String> {
    let mut system = System::new_all();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    system.refresh_process(pid);

    let tasks = &system.process(pid).unwrap().tasks;

    tasks
        .iter()
        .map(|(pid, process)| (pid.as_u32(), process.name().to_string()))
        .collect()
}

pub fn start_thread_counter_thread() {
    thread::spawn(move || loop {
        info!("Number of child processes: {}", process_task_counter());
        thread::sleep(Duration::from_secs(1));
    });
}
