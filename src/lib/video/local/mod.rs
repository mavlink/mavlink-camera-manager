#[cfg(target_os = "linux")]
pub mod video_source_local_linux;
#[cfg(not(target_os = "linux"))]
pub mod video_source_local_none;
