use anyhow::{anyhow, Context, Result};

use tracing::*;

use gst::prelude::*;

use super::SinkInterface;
use crate::stream::pipeline::runner::PipelineRunner;

#[derive(Debug)]
pub struct UdpSink {
    sink_id: uuid::Uuid,
    pipeline: gst::Pipeline,
    queue: gst::Element,
    proxysink: gst::Element,
    _proxysrc: gst::Element,
    _udpsink: gst::Element,
    udpsink_sink_pad: gst::Pad,
    tee_src_pad: Option<gst::Pad>,
    addresses: Vec<url::Url>,
    pipeline_runner: PipelineRunner,
}
impl SinkInterface for UdpSink {
    #[instrument(level = "debug", skip(self, pipeline))]
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

        // Block data flow to prevent any data before set Playing, which would cause an error
        let Some(tee_src_pad_data_blocker) = tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
        else {
            let msg =
                "Failed adding probe to Tee's src pad to block data before going to playing state"
                    .to_string();
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            return Err(anyhow!(msg));
        };

        // Add the ProxySink element to the source's pipeline
        let elements = &[&self.queue, &self.proxysink];
        if let Err(error) = pipeline.add_many(elements) {
            let msg = format!("Failed to add ProxySink to Pipeline {pipeline_id}: {error:#?}");

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            return Err(anyhow!(msg));
        }

        // Link the queue's src pad to the ProxySink's sink pad
        let queue_src_pad = &self
            .queue
            .static_pad("src")
            .expect("No src pad found on Queue");
        let proxysink_sink_pad = &self
            .proxysink
            .static_pad("sink")
            .expect("No sink pad found on ProxySink");
        if let Err(link_err) = queue_src_pad.link(proxysink_sink_pad) {
            let msg =
                format!("Failed to link Queue's src pad with ProxySink's sink pad: {link_err:?}");
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Link the new Tee's src pad to the ProxySink's sink pad
        let queue_sink_pad = &self
            .queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(link_err) = tee_src_pad.link(queue_sink_pad) {
            let msg = format!("Failed to link Tee's src pad with Queue's sink pad: {link_err:?}");
            error!(msg);

            if let Err(unlink_err) = queue_src_pad.unlink(proxysink_sink_pad) {
                error!("Failed to unlink Queue's src pad and ProxySink's sink pad: {unlink_err:?}");
            }

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Syncronize added and linked elements
        if let Err(sync_err) = pipeline.sync_children_states() {
            let msg = format!("Failed to synchronize children states: {sync_err:?}");
            error!(msg);

            if let Err(unlink_err) = queue_src_pad.unlink(proxysink_sink_pad) {
                error!("Failed to unlink Queue's src pad and ProxySink's sink pad: {unlink_err:?}");
            }

            if let Err(unlink_err) = queue_src_pad.unlink(proxysink_sink_pad) {
                error!("Failed to unlink Queue's src pad and ProxySink's sink pad: {unlink_err:?}");
            }

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Unblock data to go through this added Tee src pad
        tee_src_pad.remove_probe(tee_src_pad_data_blocker);

        Ok(())
    }

    #[instrument(level = "debug", skip(self, pipeline))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink Sink from a pipeline without a Tee src pad.");
            return Ok(());
        };

        // Block data flow to prevent any data from holding the Pipeline elements alive
        if tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
            .is_none()
        {
            warn!(
                "Failed adding probe to Tee's src pad to block data before going to playing state"
            );
        }

        // Unlink the Queue element from the source's pipeline Tee's src pad
        let queue_sink_pad = self
            .queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(unlink_err) = tee_src_pad.unlink(&queue_sink_pad) {
            warn!("Failed unlinking UdpSink's Queue element from Tee's src pad: {unlink_err:?}");
        }
        drop(queue_sink_pad);

        // Release Tee's src pad
        if let Some(parent) = tee_src_pad.parent_element() {
            parent.release_request_pad(tee_src_pad)
        }

        // Remove the Sink's elements from the Source's pipeline
        let elements = &[&self.queue, &self.proxysink];
        if let Err(remove_err) = pipeline.remove_many(elements) {
            warn!("Failed removing UdpSink's elements from pipeline: {remove_err:?}");
        }

        // Set Sink's pipeline to null
        if let Err(state_err) = self.pipeline.set_state(gst::State::Null) {
            warn!("Failed to set Pipeline's state from UdpSink to NULL: {state_err:#?}");
        }

        // Set Queue to null
        if let Err(state_err) = self.queue.set_state(gst::State::Null) {
            warn!("Failed to set Queue's state to NULL: {state_err:#?}");
        }

        // Set ProxySink to null
        if let Err(state_err) = self.proxysink.set_state(gst::State::Null) {
            warn!("Failed to set ProxySink's state to NULL: {state_err:#?}");
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

        let url = self.addresses.first().context("Missing address")?.clone();
        sdp_media.add_connection("IN", "IP4", url.host_str().context("Missing host")?, 127, 1);
        sdp_media.set_port_info(url.port().context("Missing port")? as u32, 1);
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

        if let Ok(sdp_str) = sdp.as_text() {
            debug!("Got the SDPMessage: {sdp:#?}\n\n..Which as text is: {sdp_str:?}\n\n");
        };

        Ok(sdp)
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        self.pipeline_runner.start()
    }

    #[instrument(level = "debug", skip(self))]
    fn eos(&self) {
        let pipeline_weak = self.pipeline.downgrade();
        std::thread::spawn(move || {
            let pipeline = pipeline_weak.upgrade().unwrap();
            if let Err(error) = pipeline.post_message(gst::message::Eos::new()) {
                error!("Failed posting Eos message into Sink bus. Reason: {error:?}");
            }
        });
    }
}

impl UdpSink {
    #[instrument(level = "debug")]
    pub fn try_new(sink_id: uuid::Uuid, addresses: Vec<url::Url>) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("silent", true)
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        // Create a pair of proxies. The proxysink will be used in the source's pipeline,
        // while the proxysrc will be used in this sink's pipeline
        let proxysink = gst::ElementFactory::make("proxysink").build()?;
        let _proxysrc = gst::ElementFactory::make("proxysrc")
            .property("proxysink", &proxysink)
            .build()?;

        // Configure proxysrc's queue, skips if fails
        match _proxysrc.downcast_ref::<gst::Bin>() {
            Some(bin) => {
                let elements = bin.children();
                match elements
                    .iter()
                    .find(|element| element.name().starts_with("queue"))
                {
                    Some(element) => {
                        element.set_property_from_str("leaky", "downstream"); // Throw away any data
                        element.set_property("flush-on-eos", true);
                        element.set_property("max-size-buffers", 0u32); // Disable buffers
                    }
                    None => {
                        warn!("Failed to customize proxysrc's queue: Failed to find queue in proxysrc");
                    }
                }
            }
            None => {
                warn!("Failed to customize proxysrc's queue: Failed to downcast element to bin")
            }
        }

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
        let description = format!("multiudpsink sync=false clients={clients}");
        let _udpsink =
            gst::parse::launch(&description).context("Failed parsing pipeline description")?;

        let udpsink_sink_pad = _udpsink
            .static_pad("sink")
            .context("Failed to get Sink Pad")?;

        // Create the pipeline
        let pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-sink-{sink_id}"))
            .build();

        // Add Sink elements to the Sink's Pipeline
        let elements = [&_proxysrc, &_udpsink];
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding UdpSink's elements to Sink Pipeline: {add_err:?}"
            ));
        }

        // Link Sink's elements
        if let Err(link_err) = gst::Element::link_many(elements) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from UdpSink Pipeline: {remove_err:?}")
            };
            return Err(anyhow!("Failed linking UdpSink's elements: {link_err:?}"));
        }

        let pipeline_runner = PipelineRunner::try_new(&pipeline, &sink_id, false)?;

        // Start the pipeline
        if let Err(state_err) = pipeline.set_state(gst::State::Playing) {
            return Err(anyhow!(
                "Failed starting UdpSink's pipeline: {state_err:#?}"
            ));
        }

        Ok(Self {
            sink_id,
            pipeline,
            queue,
            proxysink,
            _proxysrc,
            _udpsink,
            udpsink_sink_pad,
            addresses,
            tee_src_pad: Default::default(),
            pipeline_runner,
        })
    }
}
