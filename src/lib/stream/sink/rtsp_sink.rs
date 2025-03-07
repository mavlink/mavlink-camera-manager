use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tracing::*;

use crate::stream::rtsp::rtsp_scheme::RTSPScheme;

use super::{link_sink_to_tee, unlink_sink_from_tee, SinkInterface};

#[derive(Debug)]
pub struct RtspSink {
    sink_id: Arc<uuid::Uuid>,
    queue: gst::Element,
    sink: gst::Element,
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
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        let _ = std::fs::remove_file(&self.socket_path); // Remove if already exists

        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from Sink {:?} has already been configured",
                self.get_id()
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        let elements = &[&self.queue, &self.sink];
        link_sink_to_tee(tee_src_pad, pipeline, elements)?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self, pipeline))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &Arc<uuid::Uuid>) -> Result<()> {
        if let Err(error) = std::fs::remove_file(&self.socket_path) {
            warn!("Failed removing the RTSP Sink socket file. Reason: {error:?}");
        }

        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink Sink from a pipeline without a Tee src pad.");
            return Ok(());
        };

        let elements = &[&self.queue, &self.sink];
        unlink_sink_from_tee(tee_src_pad, pipeline, elements)?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn get_id(&self) -> Arc<uuid::Uuid> {
        self.sink_id.clone()
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
    pub fn try_new(id: Arc<uuid::Uuid>, addresses: Vec<url::Url>) -> Result<Self> {
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

        Ok(Self {
            sink_id: id.clone(),
            queue,
            sink,
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
