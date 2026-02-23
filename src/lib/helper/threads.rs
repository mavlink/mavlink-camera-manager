use std::{collections::HashMap, thread, time::Duration};

use cached::proc_macro::cached;
use sysinfo::{PidExt, ProcessExt, System, SystemExt};
use tracing::*;

/// Set the calling thread to a lower scheduling priority (nice 10) so that
/// GStreamer pipeline threads — which run at `SCHED_RR` realtime when
/// `CAP_SYS_NICE` is available — are always preferred by the OS scheduler.
#[inline]
pub fn lower_thread_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cached(time = 1)]
pub fn process_task_counter() -> usize {
    let mut system = System::new_all();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    system.refresh_process(pid);

    #[cfg(target_os = "linux")]
    {
        system.process(pid).unwrap().tasks.len()
    }

    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cached(time = 1)]
pub fn process_tasks() -> HashMap<u32, String> {
    let mut system = System::new_all();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    system.refresh_process(pid);

    #[cfg(target_os = "linux")]
    {
        let tasks = &system.process(pid).unwrap().tasks;
        tasks
            .iter()
            .map(|(pid, process)| (pid.as_u32(), process.name().to_string()))
            .collect()
    }

    #[cfg(not(target_os = "linux"))]
    {
        HashMap::new()
    }
}

pub fn start_thread_counter_thread() {
    thread::spawn(move || loop {
        info!("Number of child processes: {}", process_task_counter());
        thread::sleep(Duration::from_secs(1));
    });
}
