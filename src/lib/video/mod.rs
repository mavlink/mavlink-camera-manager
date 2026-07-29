pub(crate) mod local;

pub mod types;
pub mod video_source;
pub mod xml;

pub mod video_source_gst;
pub mod video_source_local;
pub mod video_source_onvif;
pub mod video_source_redirect;

pub mod gst_device_monitor;

/// Env var / dump helpers for the libcamera formats child-process path.
#[cfg(target_os = "linux")]
pub mod libcamera_formats {
    pub use super::local::libcamera_formats::{DUMP_ENV, dump_to_stdout};
}
