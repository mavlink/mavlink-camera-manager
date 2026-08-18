use std::{collections::HashMap, thread, time::Duration};

use cached::proc_macro::cached;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::*;

#[cached(time = 1)]
pub fn process_task_counter() -> usize {
    let mut system = System::new();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    refresh_tasks_of(&mut system, &[pid]);

    #[cfg(target_os = "linux")]
    {
        system
            .process(pid)
            .unwrap()
            .tasks()
            .map_or(0, |tasks| tasks.len())
    }

    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cached(time = 1)]
pub fn process_tasks() -> HashMap<u32, String> {
    let mut system = System::new();
    let pid = sysinfo::get_current_pid().expect("Failed to get current PID.");
    refresh_tasks_of(&mut system, &[pid]);

    #[cfg(target_os = "linux")]
    {
        let Some(tasks) = system
            .process(pid)
            .unwrap()
            .tasks()
            .map(|tasks| tasks.iter().copied().collect::<Vec<_>>())
        else {
            return HashMap::new();
        };

        // Task names are only populated by refreshing each task as a process.
        refresh_tasks_of(&mut system, &tasks);

        tasks
            .iter()
            .filter_map(|task| {
                let process = system.process(*task)?;
                Some((task.as_u32(), process.name().to_string_lossy().to_string()))
            })
            .collect()
    }

    #[cfg(not(target_os = "linux"))]
    {
        HashMap::new()
    }
}

/// Refreshes only the given processes: refreshing all of them (as
/// `System::new_all` does) costs milliseconds of `/proc` walking per call.
fn refresh_tasks_of(system: &mut System, pids: &[sysinfo::Pid]) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(pids),
        false,
        ProcessRefreshKind::nothing(),
    );
}

pub fn start_thread_counter_thread() {
    thread::spawn(move || {
        loop {
            info!("Number of child processes: {}", process_task_counter());
            thread::sleep(Duration::from_secs(1));
        }
    });
}

/// Set the calling thread to a lower scheduling priority (nice 10) so that
/// GStreamer pipeline threads -- which run at `SCHED_RR` realtime when
/// `CAP_SYS_NICE` is available -- are always preferred by the OS scheduler.
#[inline]
pub fn lower_thread_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

/// Reset the calling thread to `SCHED_OTHER` (normal scheduling) and set
/// nice 19 (lowest priority). Use this for background GStreamer pipelines
/// (e.g. thumbnail generation) that must never preempt video stream threads.
#[inline]
pub fn lower_to_background_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        libc::sched_setscheduler(0, libc::SCHED_OTHER, &param);
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
}
