pub mod image_sink;
pub mod rtsp_sink;
pub mod types;
pub mod udp_sink;
pub mod webrtc_sink;
pub mod zenoh_sink;

use anyhow::{Context, Result};
use enum_dispatch::enum_dispatch;
use gst::prelude::*;
use std::{ops::Deref, sync::Arc};
use tracing::*;

use crate::stream::types::CaptureConfiguration;
use crate::video_stream::types::VideoAndStreamInformation;

use image_sink::ImageSink;
use rtsp_sink::RtspSink;
use udp_sink::UdpSink;
use webrtc_sink::WebRTCSink;
use zenoh_sink::ZenohSink;

const FALLBACK_RTP_QUEUE_TIME_NS: u64 = 100_000_000; // 100ms

/// Compute the max-size-time (in nanoseconds) for queues that carry
/// individual RTP packets.  Uses one frame period when the stream's
/// FPS is known, otherwise falls back to 100 ms.
pub fn rtp_queue_max_time_ns(info: &VideoAndStreamInformation) -> u64 {
    if let CaptureConfiguration::Video(cfg) = &info.stream_information.configuration {
        let fi = &cfg.frame_interval;
        if fi.denominator > 0 && fi.numerator > 0 {
            let ns = (fi.numerator as u64) * 1_000_000_000 / (fi.denominator as u64);
            if ns > 0 {
                return ns;
            }
        }
    }
    FALLBACK_RTP_QUEUE_TIME_NS
}

#[enum_dispatch]
pub trait SinkInterface {
    /// Link this Sink's sink pad to the given Pipelines's Tee element's src pad.
    /// Read important notes about dynamically pipeline manipulation [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()>;

    /// Unlink this Sink's sink pad from the already associated Pipelines's Tee element's src pad.
    /// Read important notes about dynamically pipeline manipulation [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &Arc<uuid::Uuid>) -> Result<()>;

    /// Get the id associated with this Sink
    fn get_id(&self) -> Arc<uuid::Uuid>;

    /// Get the sdp file describing this Sink, following the [RFC 8866](https://www.rfc-editor.org/rfc/rfc8866.html)
    ///
    /// For a better grasp of SDP parameters, read [here](https://www.iana.org/assignments/sdp-parameters/sdp-parameters.xhtml)
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage>;

    /// Start the Sink
    fn start(&self) -> Result<()>;

    /// Terminates the Sink
    fn eos(&self);

    // Get inner pipeline
    fn pipeline(&self) -> Option<&gst::Pipeline>;
}

#[enum_dispatch(SinkInterface)]
#[derive(Debug)]
pub enum Sink {
    Udp(UdpSink),
    Rtsp(RtspSink),
    WebRTC(WebRTCSink),
    Image(ImageSink),
    Zenoh(ZenohSink),
}

impl std::fmt::Display for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sink_id = self.get_id();
        let sink_id = sink_id.deref();
        match self {
            Sink::Udp(_) => write!(f, "UdpSink sink_id={sink_id}"),
            Sink::Rtsp(_) => write!(f, "RtspSink sink_id={sink_id}"),
            Sink::WebRTC(_) => write!(f, "WebRTCSink sink_id={sink_id}"),
            Sink::Image(_) => write!(f, "ImageSink sink_id={sink_id}"),
            Sink::Zenoh(_) => write!(f, "ZenohSink sink_id={sink_id}"),
        }
    }
}

#[instrument(level = "debug", skip_all)]
pub fn create_udp_sink(
    id: Arc<uuid::Uuid>,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    Ok(Sink::Udp(UdpSink::try_new(
        id,
        video_and_stream_information,
    )?))
}

#[instrument(level = "debug", skip_all)]
pub fn create_rtsp_sink(
    id: Arc<uuid::Uuid>,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    let addresses = video_and_stream_information
        .stream_information
        .endpoints
        .clone();
    let rtp_queue_time_ns = rtp_queue_max_time_ns(video_and_stream_information);

    Ok(Sink::Rtsp(RtspSink::try_new(
        id,
        addresses,
        rtp_queue_time_ns,
    )?))
}

#[instrument(level = "debug", skip_all)]
pub fn create_image_sink(
    id: Arc<uuid::Uuid>,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    Ok(Sink::Image(ImageSink::try_new(
        id,
        video_and_stream_information,
    )?))
}

#[instrument(level = "debug", skip_all)]
pub async fn create_zenoh_sink(
    id: Arc<uuid::Uuid>,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<Sink> {
    Ok(Sink::Zenoh(
        ZenohSink::try_new(id, video_and_stream_information).await?,
    ))
}

#[instrument(level = "debug", skip_all)]
pub fn link_sink_to_tee(
    tee_src_pad: &gst::Pad,
    sink_pipeline: &gst::Pipeline,
    sink_elements: &[&gst::Element],
) -> Result<()> {
    // Block data flow to prevent any data before set Playing, which would cause an error
    let _data_blocker_guard = {
        let data_blocker_id = tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
            .context("Failed adding blocking tee_src_pad")?;

        // Unblock data to go through when the function
        scopeguard::guard(data_blocker_id, |data_blocker_id| {
            tee_src_pad.remove_probe(data_blocker_id);
        })
    };

    // Add all elements to the pipeline
    for element in sink_elements {
        let name = element.name();
        if let Err(error) = sink_pipeline.add(*element) {
            return Err(anyhow::anyhow!("Failed to add element '{name}': {error:?}"));
        }
    }

    // Link Queue to tee
    {
        let queue = sink_elements[0];
        let queue_sink_pad = queue
            .static_pad("sink")
            .expect("No Sink pad found on Queue");
        tee_src_pad
            .link(&queue_sink_pad)
            .context("Failed linking Queue to Tee")?;
    }

    // Define a cleanup guard to be run in case of errors
    let cleanup_guard = scopeguard::guard(
        (tee_src_pad, sink_pipeline),
        |(tee_src_pad, sink_pipeline)| {
            if let Some(parent) = tee_src_pad.parent_element() {
                parent.release_request_pad(tee_src_pad)
            }

            if let Err(error) = sink_pipeline.remove_many(sink_elements) {
                warn!("Failed removing elements from the pipeline: {error:?}");
            }
        },
    );

    link_and_add_all_elements(sink_pipeline, sink_elements)?;

    sink_pipeline
        .sync_children_states()
        .context("Failed synchronizing pipeline elements")?;

    // Defer the cleanup guard
    scopeguard::ScopeGuard::into_inner(cleanup_guard);

    Ok(())
}

#[instrument(level = "debug", skip_all)]
pub fn link_and_add_all_elements(
    pipeline: &gst::Pipeline,
    elements: &[&gst::Element],
) -> Result<()> {
    elements
        .windows(2)
        .enumerate()
        .try_for_each(|(position, pair)| {
            let src_element = pair[0];
            let sink_element = pair[1];

            src_element.link(sink_element).map_err(|linking_error| {
                error!("Failed linking elements at position {position:?}: {linking_error:?}");

                if let Err(unlinking_error) =
                    unlink_and_remove_all_elements(pipeline, &elements[..position])
                {
                    error!("Failed unlinking elements: {unlinking_error:?}");
                }

                linking_error
            })
        })?;

    Ok(())
}

#[instrument(level = "debug", skip_all)]
pub fn unlink_sink_from_tee(
    tee_src_pad: &gst::Pad,
    sink_pipeline: &gst::Pipeline,
    sink_elements: &[&gst::Element],
) -> Result<()> {
    // Block data flow to prevent any data before set Playing, which would cause an error
    let _data_blocker_guard = {
        let data_blocker_id = tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
            .context("Failed adding blocking tee_src_pad")?;

        // Unblock data to go through when the function
        scopeguard::guard(data_blocker_id, |data_blocker_id| {
            tee_src_pad.remove_probe(data_blocker_id);
        })
    };

    // Unlink the Queue element from the source's pipeline Tee's src pad
    {
        let queue = sink_elements[0];
        let queue_sink_pad = queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(unlink_err) = tee_src_pad.unlink(&queue_sink_pad) {
            warn!("Failed unlinking FileSink's Queue element from Tee's src pad: {unlink_err:?}");
        }
    }

    if let Some(parent) = tee_src_pad.parent_element() {
        parent.release_request_pad(tee_src_pad)
    }

    unlink_and_remove_all_elements(sink_pipeline, sink_elements)?;

    // Instead of setting each element individually to null, we are using a temporary
    // pipeline so we can post and EOS and set the state of the elements to null
    // It is important to send EOS to the queue, otherwise it can hang when setting its state to null.
    let pipeline = gst::Pipeline::new();
    pipeline.add_many(sink_elements).unwrap();
    pipeline.post_message(::gst::message::Eos::new()).unwrap();
    pipeline.set_state(gst::State::Null).unwrap();

    Ok(())
}

#[instrument(level = "debug", skip_all)]
pub fn unlink_and_remove_all_elements(
    pipeline: &gst::Pipeline,
    elements: &[&gst::Element],
) -> Result<()> {
    // Unlink every linked element
    elements.windows(2).for_each(|pair| {
        let src_element = pair[0];
        let sink_element = pair[1];

        src_element.unlink(sink_element);
    });

    // Remove all elements from the pipeline
    pipeline
        .remove_many(elements)
        .context("Failed to remove elements from pipeline")?;

    Ok(())
}
