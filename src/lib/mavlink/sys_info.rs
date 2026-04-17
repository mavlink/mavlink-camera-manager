use sysinfo::{DiskExt, System, SystemExt};
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
    // `sysinfo` reports disk space in bytes; MAVLink
    // `STORAGE_INFORMATION` and `CAMERA_CAPTURE_STATUS` expect MiB,
    // so divide by 2^20. The previous divisor (2^10) assumed KB and
    // produced values 1024× too large (e.g. QGC showing "87.48 TB"
    // for an 85 GiB free partition).
    let mut local_total_capacity: u64 = 0;
    let mut local_available_capacity: u64 = 0;

    let mut system = System::new_all();
    system.refresh_disks();

    let main_disk = system
        .disks()
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

    let boottime_ms = system.boot_time() * 1000;

    const BYTES_PER_MIB: f32 = (1u64 << 20) as f32;
    SysInfo {
        time_boot_ms: boottime_ms as u32,
        total_capacity: local_total_capacity as f32 / BYTES_PER_MIB,
        used_capacity: (local_total_capacity - local_available_capacity) as f32 / BYTES_PER_MIB,
        available_capacity: local_available_capacity as f32 / BYTES_PER_MIB,
    }
}
