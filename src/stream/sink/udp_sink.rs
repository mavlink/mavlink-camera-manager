use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use tracing::*;

use gstreamer::prelude::*;

use super::sink::{Sink, SinkInterface};

#[derive(Debug)]
pub struct UdpSink {
    sink_id: String,
    element: Arc<Mutex<gstreamer::Element>>,
    sink_pad: gstreamer::Pad,
    tee_src_pad: Option<gstreamer::Pad>,
}
impl SinkInterface for UdpSink {
    #[instrument(level = "debug")]
    fn get_element(&self) -> std::sync::MutexGuard<'_, gstreamer::Element> {
        self.element.lock().unwrap()
    }

    #[instrument(level = "debug")]
    fn get_id(&self) -> &String {
        &self.sink_id
    }

    #[instrument(level = "debug")]
    fn get_sink_pad(&self) -> &gstreamer::Pad {
        &self.sink_pad
    }

    #[instrument(level = "debug")]
    fn get_tee_src_pad(&self) -> Option<&gstreamer::Pad> {
        self.tee_src_pad.as_ref()
    }

    #[instrument(level = "debug")]
    fn set_tee_src_pad(mut self, tee_src_pad: gstreamer::Pad) -> Sink {
        self.tee_src_pad = Some(tee_src_pad);
        Sink::Udp(self)
    }
}

impl UdpSink {
    #[instrument(level = "debug")]
    pub fn try_new(id: String, addresses: Vec<url::Url>) -> Result<Self> {
        let addresses = addresses
            .iter()
            .filter_map(|address| {
                if address.scheme() != "udp" {
                    return None;
                }
                if let (Some(host), Some(port)) = (address.host(), address.port()) {
                    Some(format!("{host}:{port}"))
                } else {
                    None
                }
            })
            .collect::<Vec<String>>()
            .join(",");

        let description = format!("multiudpsink clients={addresses}");

        let element =
            gstreamer::parse_launch(&description).context("Failed parsing pipeline description")?;

        let sink_pad = element
            .sink_pads()
            .first()
            .context("Failed to get Sink Pad")?
            .clone(); // Is it safe to clone it?

        let element = Arc::new(Mutex::new(element));

        Ok(Self {
            sink_id: id,
            element,
            sink_pad,
            tee_src_pad: Default::default(),
        })
    }
}
