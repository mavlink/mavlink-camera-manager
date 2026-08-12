use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gst_app::prelude::*;
use tracing::*;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = {
        // Constructing `gst::DeviceMonitor` requires GStreamer to be initialized; ensure it is so
        // that callers like cameras_available() work even when the binary entry point hasn't run.
        gst::init().expect("Failed to initialize GStreamer");

        let manager = Manager::default();
        // An unstarted monitor has no providers, and its `devices()` silently returns nothing —
        // including during settings init, which builds default streams before `init()` runs.
        manager.monitor.set_show_all_devices(true);
        manager.monitor.set_show_all(true);
        manager
            .monitor
            .start()
            .expect("Failed to start the GStreamer device monitor");

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

#[instrument(level = "debug")]
pub fn init() {
    let manager_guard = MANAGER.lock().unwrap();

    let providers = manager_guard.monitor.providers();
    info!("GST Device Providers: {providers:#?}");
}

#[instrument(level = "debug")]
pub(crate) fn video_devices() -> Result<Vec<glib::WeakRef<gst::Device>>> {
    let monitor = &MANAGER.lock().unwrap().monitor;

    let devices = monitor
        .devices()
        .iter()
        .filter_map(|device| {
            if device.device_class().ne("Video/Source") {
                return None;
            }

            Some(device.downgrade())
        })
        .collect();

    Ok(devices)
}

#[instrument(level = "debug")]
pub fn v4l_devices() -> Result<Vec<glib::WeakRef<gst::Device>>> {
    let devices = video_devices()?
        .iter()
        .filter(|device_weak| {
            let Some(device) = device_weak.upgrade() else {
                return false;
            };

            device.properties().iter().any(|s| {
                let Ok(api) = s.get::<String>("device.api") else {
                    return false;
                };

                api.eq("v4l2")
            })
        })
        .cloned()
        .collect();

    Ok(devices)
}

#[instrument(level = "debug")]
pub fn v4l_device_with_path(device_path: &str) -> Result<glib::WeakRef<gst::Device>> {
    v4l_devices()?
        .iter()
        .find(|device_weak| {
            let Some(device) = device_weak.upgrade() else {
                return false;
            };

            device.properties().iter().any(|s| {
                let Ok(path) = s.get::<String>("device.path") else {
                    return false;
                };

                path.eq(device_path)
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_is_started_without_explicit_init() {
        // An unstarted monitor has no providers and `devices()` returns nothing. Starting in
        // the lazy_static means enumeration works before an explicit `init()` call.
        let providers = {
            let manager_guard = MANAGER.lock().unwrap();
            manager_guard.monitor.providers()
        };
        assert!(!providers.is_empty());
        assert!(video_devices().is_ok());
    }
}
