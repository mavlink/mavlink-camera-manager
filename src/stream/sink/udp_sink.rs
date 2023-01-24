use anyhow::{anyhow, Context, Result};

use tracing::*;

use gst::prelude::*;

use super::SinkInterface;
use crate::stream::pipeline::PIPELINE_SINK_TEE_NAME;

#[derive(Debug)]
pub struct UdpSink {
    sink_id: uuid::Uuid,
    queue: gst::Element,
    udpsink: gst::Element,
    udpsink_sink_pad: gst::Pad,
    tee_src_pad: Option<gst::Pad>,
    addresses: Vec<url::Url>,
}
impl SinkInterface for UdpSink {
    #[instrument(level = "debug", skip(self))]
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
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        // Add the Sink elements to the Pipeline
        let elements = &[&self.queue, &self.udpsink];
        if let Err(error) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed to add WebRTCBin {sink_id} to Pipeline {pipeline_id}. Reason: {error:#?}"
            ));
        }

        // Link the queue's src pad to the Sink's sink pad
        let queue_src_pad = &self
            .queue
            .static_pad("src")
            .expect("No sink pad found on Queue");
        if let Err(error) = queue_src_pad.link(&self.udpsink_sink_pad) {
            pipeline.remove_many(elements)?;
            return Err(anyhow!(error));
        }

        // Link the new Tee's src pad to the queue's sink pad
        let queue_sink_pad = &self
            .queue
            .static_pad("sink")
            .expect("No src pad found on Queue");
        if let Err(error) = tee_src_pad.link(queue_sink_pad) {
            pipeline.remove_many(elements)?;
            queue_src_pad.unlink(&self.udpsink_sink_pad)?;
            return Err(anyhow!(error));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let sink_id = self.get_id();

        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink sink {sink_id} from pipeline {pipeline_id} without a Tee src pad.");
            return Ok(());
        };

        let queue_sink_pad = self
            .queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(error) = tee_src_pad.unlink(&queue_sink_pad) {
            return Err(anyhow!(
                "Failed unlinking Sink {sink_id} from Tee's source pad. Reason: {error:?}"
            ));
        }
        drop(queue_sink_pad);

        let elements = &[&self.queue, &self.udpsink];
        if let Err(error) = pipeline.remove_many(elements) {
            return Err(anyhow!(
                "Failed removing UdpSrc element {sink_id} from Pipeline {pipeline_id}. Reason: {error:?}"
            ));
        }

        if let Err(error) = self.queue.set_state(gst::State::Null) {
            return Err(anyhow!(
                "Failed to set queue from sink {sink_id} state to NULL. Reason: {error:#?}"
            ));
        }

        let sink_name = format!("{PIPELINE_SINK_TEE_NAME}-{pipeline_id}");
        let tee = pipeline
            .by_name(&sink_name)
            .context(format!("no element named {sink_name:#?}"))?;
        if let Err(error) = tee.remove_pad(tee_src_pad) {
            return Err(anyhow!(
                "Failed removing Tee's source pad. Reason: {error:?}"
            ));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn get_id(&self) -> uuid::Uuid {
        self.sink_id
    }

    #[instrument(level = "debug", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        let caps = self
            .udpsink_sink_pad
            .current_caps()
            .context("Failed to get caps from UDP Sink 'sink' pad")?;
        debug!("Got caps: {caps:#?}");

        let mut sdp_media = gst_sdp::SDPMedia::new();
        gst_sdp::SDPMediaRef::set_media_from_caps(&caps, &mut sdp_media)?;

        let url = self.addresses.first().unwrap().clone();
        sdp_media.add_connection("IN", "IP4", url.host_str().expect("Missing host"), 127, 1);
        sdp_media.set_port_info(url.port().expect("Missing port") as u32, 1);
        sdp_media.set_proto("RTP/AVP");

        let mut sdp = gst_sdp::SDPMessage::new();
        sdp.add_media(sdp_media);
        sdp.set_version("0");
        sdp.set_session_name(&self.sink_id.to_string());
        sdp.set_information("This is a UDP stream");
        sdp.add_attribute(
            "tool",
            Some(&format!(
                "{} - {}",
                env!("CARGO_PKG_NAME"),
                env!("VERGEN_GIT_SHA_SHORT")
            )),
        );
        sdp.add_attribute("type", Some("broadcast"));
        sdp.add_attribute("recvonly", None);

        debug!(
            "Got the SDPMessage: {sdp:#?}\n\n..Which as text is: {:?}\n\n",
            &sdp.as_text().unwrap()
        );

        Ok(sdp)
    }
}

impl UdpSink {
    #[instrument(level = "debug")]
    pub fn try_new(id: uuid::Uuid, addresses: Vec<url::Url>) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        let clients = addresses
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
        let description = format!("multiudpsink clients={clients}");
        let udpsink =
            gst::parse_launch(&description).context("Failed parsing pipeline description")?;

        let udpsink_sink_pad = udpsink
            .static_pad("sink")
            .context("Failed to get Sink Pad")?;

        Ok(Self {
            sink_id: id,
            queue,
            udpsink,
            udpsink_sink_pad,
            addresses,
            tee_src_pad: Default::default(),
        })
    }
}
