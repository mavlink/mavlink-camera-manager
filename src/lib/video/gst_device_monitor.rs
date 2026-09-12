use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gst::prelude::*;
use tracing::*;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = {
        // Constructing `gst::DeviceMonitor` requires GStreamer to be initialized; ensure it is so
        // that callers like cameras_available() work even when the binary entry point hasn't run.
        gst::init().unwrap_or_else(|error| {
            eprintln!("Failed to initialize GStreamer: {error}");
        });

        let manager = Manager::default();
        // An unstarted monitor has no providers, and its `devices()` silently returns nothing —
        // including during settings init, which builds default streams before `init()` runs.
        manager.monitor.set_show_all_devices(true);
        manager.monitor.set_show_all(true);
        if let Err(error) = manager.monitor.start() {
            eprintln!("Failed to start the GStreamer device monitor: {error}");
        } else {
            wait_for_video_devices(&manager.monitor);
        }

        Arc::new(Mutex::new(manager))
    };
}

#[derive(Default)]
struct Manager {
    monitor: gst::DeviceMonitor,
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.monitor.stop();
    }
}

fn lock_manager() -> std::sync::MutexGuard<'static, Manager> {
    MANAGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Identify v4l2 / libcamera from GstDevice properties without `create_element`.
/// Instantiating `libcamerasrc` constructs a process-wide CameraManager; libcamera
/// aborts if a second one already exists (live stream, device provider, or probe).
pub(crate) fn source_factory_name(device: &gst::Device) -> Option<&'static str> {
    let properties = device.properties()?;
    if properties.has_field("device.path") {
        return Some("v4l2src");
    }
    if properties
        .iter()
        .any(|(name, _value)| name.as_str().starts_with("api.libcamera."))
    {
        return Some("libcamerasrc");
    }
    None
}

#[instrument(level = "debug")]
pub fn init() {
    let manager_guard = lock_manager();

    let providers = manager_guard.monitor.providers();
    info!("GST Device Providers: {providers:#?}");
}

#[instrument(level = "debug")]
pub(crate) fn video_devices() -> Result<Vec<glib::WeakRef<gst::Device>>> {
    let monitor = &lock_manager().monitor;

    let devices = monitor
        .devices()
        .iter()
        .filter_map(|device| {
            // The canonical class is "Video/Source" but libcamera-gst reports
            let class = device.device_class();
            if class.ne("Video/Source") && class.ne("Source/Video") {
                return None;
            }

            Some(device.downgrade())
        })
        .collect();

    Ok(devices)
}

#[instrument(level = "debug")]
pub fn local_devices() -> Result<Vec<glib::WeakRef<gst::Device>>> {
    let devices = video_devices()?
        .iter()
        .filter(|device_weak| {
            let Some(device) = device_weak.upgrade() else {
                return false;
            };

            let Some(factory_name) = source_factory_name(&device) else {
                return false;
            };

            matches!(factory_name, "v4l2src" | "libcamerasrc")
        })
        .cloned()
        .collect();

    Ok(devices)
}

#[instrument(level = "debug")]
pub fn local_device_with_path(device_path: &str) -> Result<glib::WeakRef<gst::Device>> {
    local_devices()?
        .iter()
        .find(|device_weak| {
            let Some(device) = device_weak.upgrade() else {
                return false;
            };

            // Match against `device.path` (v4l2-style) or the device's
            // display name (libcamera-style, e.g. "/base/soc/...").
            let by_property = device.properties().iter().any(|s| {
                s.get::<String>("device.path")
                    .map(|p| p.eq(device_path))
                    .unwrap_or(false)
            });

            by_property || device.display_name().eq(device_path)
        })
        .cloned()
        .context("Device not found")
}

#[instrument(level = "debug")]
pub fn device_caps(device: &glib::WeakRef<gst::Device>) -> Result<gst::Caps> {
    device
        .upgrade()
        .context("Fail to access device")?
        .caps()
        .context("Caps not found")
}

/// Providers probe asynchronously via the GLib main context. Pump it briefly so early
/// `cameras_available()` callers (default-stream creation) are not racing an empty list.
fn wait_for_video_devices(monitor: &gst::DeviceMonitor) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let main_context = glib::MainContext::default();
    while Instant::now() < deadline {
        while main_context.iteration(false) {}
        if monitor
            .devices()
            .iter()
            .any(|device| device.device_class() == "Video/Source")
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_is_started_without_explicit_init() {
        // An unstarted monitor has no providers and `devices()` returns nothing. Starting in
        // the lazy_static means enumeration works before an explicit `init()` call.
        let providers = {
            let manager_guard = lock_manager();
            manager_guard.monitor.providers()
        };
        assert!(!providers.is_empty());
        assert!(video_devices().is_ok());
    }
}
