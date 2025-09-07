#[cfg(target_os = "linux")]
pub mod video_source_local_linux;
#[cfg(target_os = "macos")]
pub mod video_source_local_macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub mod video_source_local_none;
