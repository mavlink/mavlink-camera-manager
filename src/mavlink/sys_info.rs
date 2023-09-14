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

    match sys_info::disk_info() {
        Ok(disk_info) => {
            local_available_capacity = disk_info.free;
            local_total_capacity = disk_info.total;
        }

        Err(error) => {
            warn!("Failed to fetch disk info: {error:#?}.");
        }
    }

    let boottime_ms = match sys_info::boottime() {
        Ok(bootime) => bootime.tv_usec / 1000,
        Err(error) => {
            warn!("Failed to fetch boottime info: {error:#?}.");
            0
        }
    };

    SysInfo {
        time_boot_ms: boottime_ms as u32,
        total_capacity: local_total_capacity as f32 / f32::powf(2.0, 10.0),
        used_capacity: ((local_total_capacity - local_available_capacity) as f32)
            / f32::powf(2.0, 10.0),
        available_capacity: local_available_capacity as f32 / f32::powf(2.0, 10.0),
    }
}
