#[cfg(target_os = "linux")]
use super::local::video_source_local_linux as source;
#[cfg(target_os = "macos")]
use super::local::video_source_local_macos as source;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use super::local::video_source_local_none as source;

pub type VideoSourceLocal = source::VideoSourceLocal;
pub type VideoSourceLocalType = source::VideoSourceLocalType;
