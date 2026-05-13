pub mod image_sink;
pub mod rtsp_sink;
pub mod types;
pub mod udp_sink;
pub mod webrtc_sink;
pub mod zenoh_sink;

use std::{ops::Deref, sync::Arc};

use anyhow::{Context, Result};
use enum_dispatch::enum_dispatch;
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::{gst::utils::wait_for_element_state, lifecycle::LifecycleState},
    video_stream::types::VideoAndStreamInformation,
};

use image_sink::ImageSink;
use rtsp_sink::RtspSink;
use udp_sink::UdpSink;
use webrtc_sink::WebRTCSink;
use zenoh_sink::ZenohSink;

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
    lifecycle: Arc<LifecycleState>,
    notify: Arc<tokio::sync::Notify>,
    persistent: Option<rtsp_sink::RtspSinkPersistent>,
) -> Result<Sink> {
    let addresses = video_and_stream_information
        .stream_information
        .endpoints
        .clone();

    Ok(Sink::Rtsp(RtspSink::try_new(
        id, addresses, lifecycle, notify, persistent,
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
    for element in sink_elements {
        force_sync_false_on_element_tree(element);
    }

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

    // Link first element to tee
    {
        let first = sink_elements[0];
        let first_sink_pad = first
            .static_pad("sink")
            .expect("No sink pad found on first element");
        tee_src_pad
            .link(&first_sink_pad)
            .context("Failed linking first element to Tee")?;
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

pub fn force_sync_false_on_element_tree(element: &gst::Element) {
    force_sync_false_on_element(element);

    if let Some(bin) = element.downcast_ref::<gst::Bin>() {
        for child in bin.iterate_recurse().into_iter().filter_map(Result::ok) {
            force_sync_false_on_element(&child);
        }
    }
}

pub fn force_sync_false_on_element(element: &gst::Element) -> bool {
    if element.find_property("sync").is_none() || element.static_pad("src").is_some() {
        return false;
    }

    element.set_property("sync", false);
    true
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
    // Block data flow while we unlink the pad from the tee.  The block
    // is scoped as tightly as possible: it is released right after the
    // request pad is freed, *before* any slow element cleanup (e.g.
    // webrtcbin set_state(Null) which may wait for ICE/DTLS teardown).
    // Keeping the block active longer would stall the tee's streaming
    // thread and starve other branches (e.g. RTSP via video_tee).
    {
        let data_blocker_id = tee_src_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
            .context("Failed adding blocking tee_src_pad")?;

        let _data_blocker_guard = scopeguard::guard(data_blocker_id, |data_blocker_id| {
            tee_src_pad.remove_probe(data_blocker_id);
        });

        // Unlink the tee src pad from whatever downstream pad it is connected to
        // (normally the queue's sink pad, but may be the webrtcbin's sink pad if
        // the queue was excised at runtime).
        if let Some(peer_pad) = tee_src_pad.peer()
            && let Err(unlink_err) = tee_src_pad.unlink(&peer_pad)
        {
            warn!("Failed unlinking tee src pad from peer: {unlink_err:?}");
        }

        if let Some(parent) = tee_src_pad.parent_element() {
            parent.release_request_pad(tee_src_pad)
        }

        // _data_blocker_guard drops here, removing the probe
    }

    // --- Everything below runs without blocking the tee ---

    let is_webrtcbin = |e: &gst::Element| {
        e.factory()
            .map(|f| f.name() == "webrtcbin")
            .unwrap_or(false)
    };

    // Transition webrtcbin to Null while still in the pipeline so the
    // pipeline's state management propagates correctly to all internal
    // children (transportsendbin, nicesink, dtlssrtpdec, …).
    // We must wait for the transition to complete: on GStreamer >= 1.28
    // the internal NiceSrc/DTLS threads do not terminate until the
    // element actually reaches Null, and removing it early leaves
    // orphaned threads.
    for elem in sink_elements.iter().filter(|e| is_webrtcbin(e)) {
        elem.send_event(gst::event::Eos::builder().build());

        if let Err(error) = elem.set_state(gst::State::Null) {
            warn!("Failed setting {} to Null: {error:?}", elem.name());
        }

        if let Err(error) = wait_for_element_state(elem.downgrade(), gst::State::Null, 100, 5) {
            warn!("{} did not reach Null within 5 s: {error:?}", elem.name());
        }
    }

    unlink_and_remove_all_elements(sink_pipeline, sink_elements)?;

    // Non-webrtcbin elements: flush via a temp pipeline.
    let simple: Vec<&gst::Element> = sink_elements
        .iter()
        .filter(|e| !is_webrtcbin(e))
        .copied()
        .collect();

    if !simple.is_empty() {
        let pipeline = gst::Pipeline::new();
        pipeline.add_many(&simple).unwrap();
        pipeline.set_state(gst::State::Ready).unwrap();
        pipeline.send_event(gst::event::Eos::builder().build());
        pipeline.set_state(gst::State::Null).unwrap();
        match pipeline.state(gst::ClockTime::from_seconds(5)) {
            (Ok(_), _, _) => {}
            (Err(error), cur, pending) => {
                warn!(
                    "Temp pipeline did not reach Null within 5 s \
                     (err={error:?}, cur={cur:?}, pending={pending:?})"
                );
            }
        }
    }

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
