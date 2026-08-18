use sysinfo::{DiskRefreshKind, Disks, System};
use tracing::*;

#[derive(Debug)]
pub struct SysInfo {
    pub time_boot_ms: u32,
    pub total_capacity: f32,
    pub used_capacity: f32,
    pub available_capacity: f32,
}

#[instrument(level = "debug")]
pub fn sys_info() -> SysInfo {
    //Both uses KB
    let mut local_total_capacity = 0;
    let mut local_available_capacity = 0;

    let disks = Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());

    let main_disk = disks
        .iter()
        .find(|disk| disk.mount_point().as_os_str() == "/");
    match main_disk {
        Some(disk_info) => {
            local_available_capacity = disk_info.available_space();
            local_total_capacity = disk_info.total_space();
        }

        None => {
            warn!("Failed to fetch main disk info.");
        }
    }

    let boottime_ms = System::boot_time() * 1000;

    SysInfo {
        time_boot_ms: boottime_ms as u32,
        total_capacity: local_total_capacity as f32 / f32::powf(2.0, 10.0),
        used_capacity: ((local_total_capacity - local_available_capacity) as f32)
            / f32::powf(2.0, 10.0),
        available_capacity: local_available_capacity as f32 / f32::powf(2.0, 10.0),
    }
}
