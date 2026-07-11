use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gst_app::prelude::*;
use tracing::*;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = {
        // Constructing `gst::DeviceMonitor` requires GStreamer to be initialized; ensure it is so
        // that callers like cameras_available() work even when the binary entry point hasn't run.
        gst::init().expect("Failed to initialize GStreamer");
        Arc::new(Mutex::new(Manager::default()))
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

#[instrument(level = "debug")]
pub fn init() -> Result<()> {
    let manager_guard = MANAGER.lock().unwrap();

    manager_guard.monitor.set_show_all_devices(true);
    manager_guard.monitor.set_show_all(true);
    manager_guard.monitor.start()?;

    let providers = manager_guard.monitor.providers();
    info!("GST Device Providers: {providers:#?}");

    Ok(())
}

#[instrument(level = "debug")]
pub(crate) fn video_devices() -> Result<Vec<glib::WeakRef<gst::Device>>> {
    let monitor = &MANAGER.lock().unwrap().monitor;

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

            // Identify by source factory rather than `device.api`, since
            // libcamera-gst does not expose `device.api`.
            let Ok(probe) = device.create_element(None) else {
                return false;
            };
            let Some(factory) = probe.factory() else {
                return false;
            };

            matches!(factory.name().as_str(), "v4l2src" | "libcamerasrc")
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
