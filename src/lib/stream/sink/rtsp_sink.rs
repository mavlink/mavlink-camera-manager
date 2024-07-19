use anyhow::{anyhow, Context, Result};

use tracing::*;

use gst::prelude::*;

use crate::stream::rtsp::rtsp_scheme::RTSPScheme;

use super::SinkInterface;

#[derive(Debug)]
pub struct RtspSink {
    sink_id: uuid::Uuid,
    queue: gst::Element,
    sink: gst::Element,
    sink_sink_pad: gst::Pad,
    tee_src_pad: Option<gst::Pad>,
    scheme: RTSPScheme,
    path: String,
    socket_path: String,
}
impl SinkInterface for RtspSink {
    #[instrument(level = "debug", skip(self, pipeline))]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        let sink_id = &self.get_id();

        let _ = std::fs::remove_file(&self.socket_path); // Remove if already exists

        // Set Tee's src pad
        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from RtspSink {sink_id} has already been configured"
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

        // Add the Sink elements to the Pipeline
        let elements = &[&self.queue, &self.sink];
        if let Err(add_err) = pipeline.add_many(elements) {
            let msg = format!("Failed to add RTSP's elements to the Pipeline: {add_err:?}");
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            return Err(anyhow!(msg));
        }

        // Link the queue's src pad to the Sink's sink pad
        let queue_src_pad = &self
            .queue
            .static_pad("src")
            .expect("No sink pad found on Queue");
        if let Err(link_err) = queue_src_pad.link(&self.sink_sink_pad) {
            let msg =
                format!("Failed to link Queue's src pad with RtspSink's sink pad: {link_err:?}");
            error!(msg);

            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(remove_err) = pipeline.remove_many(elements) {
                error!("Failed to remove elements from pipeline: {remove_err:?}");
            }

            return Err(anyhow!(msg));
        }

        // Link the new Tee's src pad to the queue's sink pad
        let queue_sink_pad = &self
            .queue
            .static_pad("sink")
            .expect("No src pad found on Queue");
        if let Err(link_err) = tee_src_pad.link(queue_sink_pad) {
            let msg = format!("Failed to link Tee's src pad with Queue's sink pad: {link_err:?}");
            error!(msg);

            if let Err(unlink_err) = queue_src_pad.unlink(&self.sink_sink_pad) {
                error!("Failed to unlink Queue's src pad from RtspSink's sink pad: {unlink_err:?}");
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

            if let Err(unlink_err) = queue_src_pad.unlink(&self.sink_sink_pad) {
                error!("Failed to unlink Queue's src pad from RtspSink's sink pad: {unlink_err:?}");
            }

            if let Err(unlink_err) = tee_src_pad.unlink(queue_sink_pad) {
                error!("Failed to unlink Tee's src pad and Qeueu's sink pad: {unlink_err:?}");
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
        if let Err(error) = std::fs::remove_file(&self.socket_path) {
            warn!("Failed removing the RTSP Sink socket file. Reason: {error:?}");
        }

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
            warn!("Failed unlinking WebRTC's Queue element from Tee's src pad: {unlink_err:?}");
        }
        drop(queue_sink_pad);

        // Release Tee's src pad
        if let Some(parent) = tee_src_pad.parent_element() {
            parent.release_request_pad(tee_src_pad)
        }

        // Remove the Sink's elements from the Source's pipeline
        let elements = &[&self.queue, &self.sink];
        if let Err(remove_err) = pipeline.remove_many(elements) {
            warn!("Failed removing RtspSink's elements from pipeline: {remove_err:?}");
        }

        // Set Queue to null
        if let Err(state_err) = self.queue.set_state(gst::State::Null) {
            warn!("Failed to set Queue's state to NULL: {state_err:#?}");
        }

        // Set Sink to null
        if let Err(state_err) = self.sink.set_state(gst::State::Null) {
            warn!("Failed to set RtspSink's to NULL: {state_err:#?}");
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn get_id(&self) -> uuid::Uuid {
        self.sink_id
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available. Reason: RTSP Sink should only be connected from its RTSP endpoint."
        ))
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn eos(&self) {}
}

impl RtspSink {
    #[instrument(level = "debug")]
    pub fn try_new(id: uuid::Uuid, addresses: Vec<url::Url>) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("silent", true)
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        let (path, scheme) = addresses
            .iter()
            .find_map(|address| {
                address
                    .scheme()
                    .starts_with("rtsp")
                    .then_some(RTSPScheme::try_from(address.scheme()).unwrap_or_default())
                    .map(|scheme| (address.path().to_string(), scheme))
            })
            .context(
                "Failed to find RTSP compatible address. Example: \"rtsp://0.0.0.0:8554/test\"",
            )?;

        let socket_path = format!("/tmp/{id}");
        let sink = gst::ElementFactory::make("shmsink")
            .property_from_str("socket-path", &socket_path)
            .property("sync", false)
            .property("wait-for-connection", false)
            .property("shm-size", 10_000_000u32)
            .property("enable-last-sample", false)
            .build()?;

        let sink_sink_pad = sink.static_pad("sink").context("Failed to get Sink Pad")?;

        Ok(Self {
            sink_id: id,
            queue,
            sink,
            sink_sink_pad,
            scheme,
            path,
            socket_path,
            tee_src_pad: Default::default(),
        })
    }

    #[instrument(level = "trace", skip(self))]
    pub fn path(&self) -> String {
        self.path.clone()
    }

    #[instrument(level = "trace", skip(self))]
    pub fn scheme(&self) -> RTSPScheme {
        self.scheme.clone()
    }

    #[instrument(level = "trace", skip(self))]
    pub fn socket_path(&self) -> String {
        self.socket_path.clone()
    }
}
