use anyhow::{anyhow, Context, Result};

use tracing::*;

use gst::prelude::*;

use super::sink::SinkInterface;
use crate::stream::pipeline::pipeline::PIPELINE_TEE_NAME;

#[derive(Debug)]
pub struct UdpSink {
    sink_id: uuid::Uuid,
    element: gst::Element,
    sink_pad: gst::Pad,
    tee_src_pad: Option<gst::Pad>,
}
impl SinkInterface for UdpSink {
    #[instrument(level = "debug")]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        let sink_id = &self.get_id();

        // Set Tee's src pad
        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from UdpSink {sink_id} has already been configured"
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);

        // Add the Sink element to the Pipeline
        let sink_element = &self.element;
        if let Err(error) = pipeline.add(sink_element) {
            bail!("Failed to add UdpSink {sink_id} to Pipeline {pipeline_id}. Reason: {error:#?}");
        }

        // Link the new Tee's src pad to the Sink's sink pad
        if let Some(tee_src_pad) = &self.tee_src_pad {
            if let Err(error) = tee_src_pad.link(&self.sink_pad) {
                pipeline.remove(sink_element)?;
                bail!(error)
            }
        }

        // Set state
        if let Err(error) = sink_element.sync_state_with_parent() {
            pipeline.remove(sink_element)?;
            bail!(error)
        }

        Ok(())
    }

    #[instrument(level = "debug")]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let sink_id = self.get_id();

        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink sink {sink_id} from pipeline {pipeline_id} without a Tee src pad.");
            return Ok(());
        };

        if let Err(error) = tee_src_pad.unlink(&self.sink_pad) {
            bail!("Failed unlinking Sink {sink_id} from Tee's source pad. Reason: {error:?}");
        }

        let sink_element = &self.element;
        if let Err(error) = pipeline.remove(sink_element) {
            bail!(
                "Failed removing UdpSink element {sink_id} from Pipeline {pipeline_id}. Reason: {error:?}"
            );
        }

        // TODO: This is causing a panic on this thread
        // if let Err(error) = sink_element.set_state(gst::State::Null) {
        //     bail!("Failed to set UdpSink state to NULL. Reason: {error:#?}")
        // }

        let tee = pipeline
            .by_name(PIPELINE_TEE_NAME)
            .context(format!("no element named {PIPELINE_TEE_NAME:#?}"))?;
        if let Err(error) = tee.remove_pad(tee_src_pad) {
            return Err(anyhow!(
                "Failed removing Tee's source pad. Reason: {error:?}"
            ));
        }

        Ok(())
    }

    #[instrument(level = "debug")]
    fn get_id(&self) -> uuid::Uuid {
        self.sink_id.clone()
    }
}

impl UdpSink {
    #[instrument(level = "debug")]
    pub fn try_new(id: uuid::Uuid, addresses: Vec<url::Url>) -> Result<Self> {
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
            gst::parse_launch(&description).context("Failed parsing pipeline description")?;

        let sink_pad = element
            .sink_pads()
            .first()
            .context("Failed to get Sink Pad")?
            .clone(); // Is it safe to clone it?

        Ok(Self {
            sink_id: id,
            element,
            sink_pad,
            tee_src_pad: Default::default(),
        })
    }
}
