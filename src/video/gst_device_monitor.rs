use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gst_app::prelude::DeviceMonitorExt;
use tracing::*;

use gst::{self, prelude::*};

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

#[derive(Default)]
struct Manager {
    monitor: gst::DeviceMonitor,
}

#[instrument(level = "debug")]
pub fn init() -> Result<()> {
    if let Err(error) = gst::init() {
        error!("Error! {error}");
    };

    let monitor = &MANAGER.lock().unwrap().monitor;

    let bus: gst::Bus = monitor.bus();
    bus.add_watch(move |_bus, message| {
        use gst::MessageView::*;

        match message.view() {
            DeviceAdded(message) => {
                let device = message.device();
                debug!("Device added: {device:#?}");
            }
            DeviceRemoved(message) => {
                let device = message.device();
                debug!("Device removed: {device:#?}");
            }
            DeviceChanged(message) => {
                let (old, new) = message.device_changed();
                debug!("Device changed from: {old:#?} to {new:#?}");
            }
            _ => (),
        }

        Continue(true)
    })?;

    monitor.set_show_all_devices(true);
    // monitor.set_show_all(true);

    monitor.start()?;

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
pub fn devices() -> Result<Vec<gst::Device>> {
    let monitor = &MANAGER.lock().unwrap().monitor;

    let devices = monitor.devices().iter().cloned().collect();

    Ok(devices)
}

#[instrument(level = "debug")]
pub fn v4l_devices() -> Result<Vec<gst::Device>> {
    let devices = devices()?
        .iter()
        .filter(|device| {
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
pub fn device_with_path(device_path: &str) -> Result<gst::Device> {
    v4l_devices()?
        .iter()
        .find(|device| {
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
pub fn device_caps(device: &gst::Device) -> Result<gst::Caps> {
    device.caps().context("Caps not found")
}
