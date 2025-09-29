use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gst_app::prelude::*;
use tokio::task::JoinHandle;
use tracing::*;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

#[derive(Default)]
struct Manager {
    monitor: gst::DeviceMonitor,
    monitor_task: Option<JoinHandle<()>>,
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.monitor.stop();

        if let Some(task) = &self.monitor_task {
            task.abort();
        }
    }
}

#[instrument(level = "debug")]
pub fn init() -> Result<()> {
    if let Err(error) = gst::init() {
        error!("Error! {error}");
    };

    let mut manager_guard = MANAGER.lock().unwrap();

    let (bus_tx, mut bus_rx) = tokio::sync::mpsc::unbounded_channel::<gst::Message>();
    let bus = manager_guard.monitor.bus();
    bus.set_sync_handler(move |_, msg| {
        let _ = bus_tx.send(msg.to_owned());
        gst::BusSyncReply::Pass
    });

    manager_guard.monitor_task = Some(tokio::spawn(async move {
        use gst::MessageView::*;

        while let Some(message) = bus_rx.recv().await {
            match message.view() {
                DeviceAdded(message) => {
                    let device = message.device();

                    if device.device_class().ne("Video/Source") {
                        continue;
                    }

                    let name = device.display_name();

                    use gst_app::prelude::*;
                    let properties = device.properties();
                    let caps = device.caps();
                    info!("Device added: {name:?}, properties: {properties:#?}, caps: {caps:#?}");
                }
                DeviceRemoved(message) => {
                    let device = message.device();

                    if device.device_class().ne("Video/Source") {
                        continue;
                    }

                    let name = device.display_name();
                    info!("Device removed: {name:?}");
                }
                DeviceChanged(message) => {
                    let (old, new) = message.device_changed();

                    if new.device_class().ne("Video/Source") {
                        continue;
                    }

                    let old_name = old.display_name();
                    let new_name = new.display_name();
                    info!("Device changed from: {old_name:?} to {new_name:?}");
                }
                _ => (),
            }
        }
    }));

    manager_guard.monitor.set_show_all_devices(true);
    manager_guard.monitor.set_show_all(true);
    manager_guard.monitor.start()?;

    let providers = manager_guard.monitor.providers();
    info!("GST Device Providers: {providers:#?}");

    Ok(())
}

#[instrument(level = "debug")]
pub fn providers() -> Result<()> {
    let monitor = &MANAGER.lock().unwrap().monitor;

    let providers = monitor.providers();

    providers.iter().for_each(|provider| {
        debug!("Provider: {provider:#?}");
    });

    Ok(())
}

#[instrument(level = "debug")]
pub fn video_devices() -> Result<Vec<glib::WeakRef<gst::Device>>> {
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
