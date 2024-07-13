#[cfg(target_os = "linux")]
use super::local::video_source_local_linux as source;
#[cfg(not(target_os = "linux"))]
use super::local::video_source_local_none as source;

pub type VideoSourceLocal = source::VideoSourceLocal;
pub type VideoSourceLocalType = source::VideoSourceLocalType;
