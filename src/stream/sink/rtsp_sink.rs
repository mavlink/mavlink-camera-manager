use anyhow::{anyhow, Context, Result};

use tracing::*;

use gst::prelude::*;

use super::SinkInterface;
use crate::stream::pipeline::PIPELINE_SINK_TEE_NAME;

#[derive(Debug)]
pub struct RtspSink {
    sink_id: uuid::Uuid,
    queue: gst::Element,
    sink: gst::Element,
    sink_sink_pad: gst::Pad,
    tee_src_pad: Option<gst::Pad>,
    path: String,
    socket_path: String,
}
impl SinkInterface for RtspSink {
    #[instrument(level = "debug", skip(self))]
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

        // Add the Sink elements to the Pipeline
        let elements = &[&self.queue, &self.sink];
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
        if let Err(error) = queue_src_pad.link(&self.sink_sink_pad) {
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
            queue_src_pad.unlink(&self.sink_sink_pad)?;
            return Err(anyhow!(error));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let sink_id = self.get_id();

        if let Err(error) = std::fs::remove_file(&self.socket_path) {
            warn!("Failed removing the RTSP Sink socket file. Reason: {error:?}");
        }

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

        let elements = &[&self.queue, &self.sink];
        if let Err(error) = pipeline.remove_many(elements) {
            return Err(anyhow!(
                "Failed removing RtspSrc element {sink_id} from Pipeline {pipeline_id}. Reason: {error:?}"
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

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available. Reason: RTSP Sink should only be connected from its RTSP endpoint."
        ))
    }
}

impl RtspSink {
    #[instrument(level = "debug")]
    pub fn try_new(id: uuid::Uuid, addresses: Vec<url::Url>) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        let path = addresses
            .iter()
            .find_map(|address| (address.scheme() == "rtsp").then_some(address.path().to_string()))
            .context(
                "Failed to find RTSP compatible address. Example: \"rtsp://0.0.0.0:8554/test\"",
            )?;

        let socket_path = format!("/tmp/{id}");
        let sink = gst::ElementFactory::make("shmsink")
            .property_from_str("socket-path", &socket_path)
            .property("sync", true)
            .property("wait-for-connection", false)
            .property("shm-size", 10_000_000u32)
            .build()?;

        let sink_sink_pad = sink.static_pad("sink").context("Failed to get Sink Pad")?;

        Ok(Self {
            sink_id: id,
            queue,
            sink,
            sink_sink_pad,
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
    pub fn socket_path(&self) -> String {
        self.socket_path.clone()
    }
}
