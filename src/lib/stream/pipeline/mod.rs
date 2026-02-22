pub mod fake_pipeline;
pub mod onvif_pipeline;
pub mod qr_pipeline;
pub mod redirect_pipeline;
pub mod runner;
#[cfg(target_os = "linux")]
pub mod v4l_pipeline;

use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, Context, Result};
use enum_dispatch::enum_dispatch;
use gst::prelude::*;
use tracing::*;

use mcm_api::v1::{stream::VideoAndStreamInformation, video::VideoSourceType};

use crate::stream::stats::pipeline_analysis::SinkInfo;

use crate::stream::{
    gst::utils::wait_for_element_state_async,
    rtsp::rtsp_server::RTSPServer,
    sink::{Sink, SinkInterface},
    stats::pipeline_analysis,
};

use fake_pipeline::FakePipeline;
use onvif_pipeline::OnvifPipeline;
use qr_pipeline::QrPipeline;
use redirect_pipeline::RedirectPipeline;
use runner::PipelineRunner;

#[cfg(target_os = "linux")]
use v4l_pipeline::V4lPipeline;

#[enum_dispatch]
pub trait PipelineGstreamerInterface {
    fn is_running(&self) -> bool;
}

#[enum_dispatch(PipelineGstreamerInterface)]
#[derive(Debug)]
pub enum Pipeline {
    #[cfg(target_os = "linux")]
    V4l(V4lPipeline),
    Fake(FakePipeline),
    QR(QrPipeline),
    Onvif(OnvifPipeline),
    Redirect(RedirectPipeline),
}

impl Pipeline {
    pub fn inner_state_mut(&mut self) -> &mut PipelineState {
        match self {
            #[cfg(target_os = "linux")]
            Pipeline::V4l(pipeline) => &mut pipeline.state,
            Pipeline::Fake(pipeline) => &mut pipeline.state,
            Pipeline::QR(pipeline) => &mut pipeline.state,
            Pipeline::Onvif(pipeline) => &mut pipeline.state,
            Pipeline::Redirect(pipeline) => &mut pipeline.state,
        }
    }

    pub fn inner_state_as_ref(&self) -> &PipelineState {
        match self {
            #[cfg(target_os = "linux")]
            Pipeline::V4l(pipeline) => &pipeline.state,
            Pipeline::Fake(pipeline) => &pipeline.state,
            Pipeline::QR(pipeline) => &pipeline.state,
            Pipeline::Onvif(pipeline) => &pipeline.state,
            Pipeline::Redirect(pipeline) => &pipeline.state,
        }
    }

    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        pipeline_id: &Arc<uuid::Uuid>,
    ) -> Result<Self> {
        let pipeline_state: PipelineState =
            PipelineState::try_new(video_and_stream_information, pipeline_id)?;
        Ok(match &video_and_stream_information.video_source {
            VideoSourceType::Gst(video_source_gst) => match video_source_gst.source {
                mcm_api::v1::video::VideoSourceGstType::Local(_) => todo!(),
                mcm_api::v1::video::VideoSourceGstType::Fake(_) => Pipeline::Fake(FakePipeline {
                    state: pipeline_state,
                }),
                mcm_api::v1::video::VideoSourceGstType::QR(_) => Pipeline::QR(QrPipeline {
                    state: pipeline_state,
                }),
            },
            #[cfg(target_os = "linux")]
            VideoSourceType::Local(_) => Pipeline::V4l(V4lPipeline {
                state: pipeline_state,
            }),
            #[cfg(not(target_os = "linux"))]
            VideoSourceType::Local(_) => unreachable!("Local is only supported on linux"),
            VideoSourceType::Onvif(_) => Pipeline::Onvif(OnvifPipeline {
                state: pipeline_state,
            }),
            VideoSourceType::Redirect(_) => Pipeline::Redirect(RedirectPipeline {
                state: pipeline_state,
            }),
        })
    }

    #[instrument(level = "debug", skip(self), fields(sink = %sink))]
    pub async fn add_sink(&mut self, sink: Sink) -> Result<()> {
        self.inner_state_mut().add_sink(sink).await
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn remove_sink(&mut self, sink_id: &uuid::Uuid) -> Result<()> {
        self.inner_state_mut().remove_sink(sink_id)
    }
}

#[derive(Debug)]
pub struct PipelineState {
    pub pipeline_id: Arc<uuid::Uuid>,
    pub pipeline: gst::Pipeline,
    pub video_tee: Option<gst::Element>,
    pub rtp_tee: Option<gst::Element>,
    pub sinks: HashMap<uuid::Uuid, Sink>,
    pub pipeline_runner: PipelineRunner,
}

pub const PIPELINE_RTP_TEE_NAME: &str = "RTPTee";
pub const PIPELINE_VIDEO_TEE_NAME: &str = "VideoTee";
pub const PIPELINE_FILTER_NAME: &str = "Filter";

impl PipelineState {
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        pipeline_id: &Arc<uuid::Uuid>,
    ) -> Result<Self> {
        let pipeline = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(video) => match video.source {
                mcm_api::v1::video::VideoSourceGstType::Local(_) => todo!(),
                mcm_api::v1::video::VideoSourceGstType::Fake(_) => {
                    FakePipeline::try_new(pipeline_id, video_and_stream_information)
                }
                mcm_api::v1::video::VideoSourceGstType::QR(_) => {
                    QrPipeline::try_new(pipeline_id, video_and_stream_information)
                }
            },
            #[cfg(target_os = "linux")]
            VideoSourceType::Local(_) => {
                V4lPipeline::try_new(pipeline_id, video_and_stream_information)
            }
            #[cfg(not(target_os = "linux"))]
            VideoSourceType::Local(_) => {
                unreachable!("Local source only supported on linux");
            }
            VideoSourceType::Onvif(_) => {
                OnvifPipeline::try_new(pipeline_id, video_and_stream_information)
            }
            VideoSourceType::Redirect(_) => {
                RedirectPipeline::try_new(pipeline_id, video_and_stream_information)
            }
        }?;

        let video_tee = pipeline.by_name(&format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}"));

        let rtp_tee = pipeline.by_name(&format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}"));

        let pipeline_runner = PipelineRunner::try_new(
            &pipeline,
            pipeline_id,
            pipeline_id,
            false,
            video_and_stream_information,
        )?;

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-created"),
        );

        Ok(Self {
            pipeline_id: pipeline_id.clone(),
            pipeline,
            video_tee,
            rtp_tee,
            sinks: Default::default(),
            pipeline_runner,
        })
    }

    /// Links the sink pad from the given Sink to this Pipeline's Tee element
    #[instrument(level = "debug", skip_all)]
    pub async fn add_sink(&mut self, mut sink: Sink) -> Result<()> {
        let pipeline_id = &self.pipeline_id;

        // Request a new src pad for the used Tee
        // Note: Here we choose if the sink will receive a Video or RTP packages
        let tee = match sink {
            Sink::Image(_) | Sink::Zenoh(_) | Sink::Rtsp(_) => &self.video_tee,
            Sink::Udp(_) | Sink::WebRTC(_) => &self.rtp_tee,
        };

        let Some(tee) = tee else {
            return Err(anyhow!("No Tee for this kind of Pipeline"));
        };

        let tee_src_pad = tee.request_pad_simple("src_%u").context(format!(
            "Failed requesting src pad for Tee of the pipeline {pipeline_id}"
        ))?;
        debug!("Got tee's src pad {:#?}", tee_src_pad.name());

        // Link the Sink
        let pipeline = &self.pipeline;
        sink.link(pipeline, pipeline_id, tee_src_pad)?;
        let sink_id = &sink.get_id();

        // Start the pipeline if not playing yet
        if pipeline.current_state() != gst::State::Playing {
            if let Err(error) = pipeline.set_state(gst::State::Playing) {
                sink.unlink(pipeline, pipeline_id)?;
                return Err(anyhow!(
                    "Failed starting Pipeline {pipeline_id}. Reason: {error:#?}"
                ));
            }
        }

        if let Err(error) = wait_for_element_state_async(
            gst::prelude::ObjectExt::downgrade(pipeline),
            gst::State::Playing,
            100,
            2,
        )
        .await
        {
            let _ = pipeline.set_state(gst::State::Null);
            sink.unlink(pipeline, pipeline_id)?;
            return Err(anyhow!(
                "Failed setting Pipeline {pipeline_id} to Playing state. Reason: {error:?}"
            ));
        }

        if let Sink::Rtsp(sink) = &sink {
            if let Some(video_tee) = &self.video_tee {
                // Prefer fully-negotiated caps (includes codec_data for H264 avc),
                // fall back to the capsfilter's configured caps for initial setup.
                let caps = video_tee
                    .static_pad("sink")
                    .and_then(|p| p.current_caps())
                    .or_else(|| {
                        let filter_name = format!("{PIPELINE_FILTER_NAME}-{}", self.pipeline_id);
                        pipeline
                            .by_name(&filter_name)
                            .and_then(|f| f.property::<Option<gst::Caps>>("caps"))
                    })
                    .context("Failed to get caps for RTSP sink")?;

                debug!("RTSP video caps: {:#?}", caps.to_string());

                // In case it exisits, try to remove it first, but skip the result
                let _ = RTSPServer::stop_pipeline(&sink.path());

                RTSPServer::add_pipeline(
                    &sink.scheme(),
                    &sink.path(),
                    sink.rtsp_appsrc(),
                    sink.pts_offset(),
                    &caps,
                    sink.rtp_queue_time_ns(),
                    sink.flow_handle(),
                )?;

                RTSPServer::start_pipeline(&sink.path())?;
            }
        }

        // Skipping ImageSink syncronization because it goes to some wrong state,
        // and all other sinks need it to work without freezing when dynamically
        // added.
        if !matches!(&sink, Sink::Image(..)) {
            if let Err(error) = pipeline.sync_children_states() {
                error!("Failed to syncronize children states. Reason: {error:?}");
            }
        }

        self.sinks.insert(**sink_id, sink);

        self.sync_sinks_to_analysis();

        Ok(())
    }

    /// Unlinks the src pad from this Sink from the given sink pad of a Tee element
    ///
    /// Important notes about pad unlinking: [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    #[instrument(level = "info", skip_all)]
    pub fn remove_sink(&mut self, sink_id: &uuid::Uuid) -> Result<()> {
        let pipeline_id = &self.pipeline_id;

        let pipeline = &self.pipeline;
        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-before-removing"),
        );

        let sink = self.sinks.remove(sink_id).context(format!(
            "Sink {sink_id} not found in Pipeline {pipeline_id}"
        ))?;

        // Terminate the Sink
        sink.eos();

        // Unlink the Sink
        sink.unlink(pipeline, pipeline_id)?;

        if let Sink::Rtsp(sink) = &sink {
            RTSPServer::stop_pipeline(&sink.path())?;
        }

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-after-removing"),
        );

        self.sync_sinks_to_analysis();

        Ok(())
    }

    /// Push the updated topology (including active sinks) to the pipeline analysis registry.
    fn sync_sinks_to_analysis(&self) {
        let pipeline_name = self.pipeline.name().to_string();
        let src_children: Vec<gst::Element> = self.pipeline.children();

        let sink_infos: Vec<SinkInfo> = self
            .sinks
            .values()
            .map(|sink| {
                let (sink_type, tee) = match sink {
                    Sink::WebRTC(_) => ("webrtc", "rtp"),
                    Sink::Rtsp(_) => ("rtsp", "video"),
                    Sink::Udp(_) => ("udp", "rtp"),
                    Sink::Image(_) => ("image", "video"),
                    Sink::Zenoh(_) => ("zenoh", "video"),
                };

                // Collect the full element chain by walking the graph.
                // For sinks with a sub-pipeline (proxysrc side), we find the bridge
                // elements (queue → proxysink) by tracing upstream from the proxysrc's
                // peer in the source pipeline. No per-sink trait method needed.
                let elements = collect_sink_elements(sink, &src_children);

                SinkInfo {
                    id: sink.get_id().to_string(),
                    sink_type: sink_type.to_string(),
                    tee: tee.to_string(),
                    elements,
                }
            })
            .collect();

        // Re-extract topology since sink elements (queue, proxysink) are now in the source pipeline,
        // and embed the active sinks into it.
        // Note: probe installation for new elements is handled automatically
        // via the element-added signal connected in install_probes().
        let mut topo = runner::extract_topology(&self.pipeline);
        topo.sinks = sink_infos;
        pipeline_analysis::update_pipeline_topology(&pipeline_name, topo);
    }
}

/// Collect the full element chain for a sink in source→sink order.
///
/// For sinks with a sub-pipeline (proxy-based or shm-based: udp, image, zenoh),
/// finds the bridge elements (queue → proxysink/shmsink) by tracing upstream from
/// the sub-pipeline's source element through the source pipeline, then appends the
/// sub-pipeline elements.
///
/// For sinks without a sub-pipeline (webrtc, rtsp), finds the elements by looking
/// for known element names in the source pipeline children.
fn collect_sink_elements(sink: &Sink, src_children: &[gst::Element]) -> Vec<String> {
    if let Some(sub_pipeline) = sink.pipeline() {
        // Sink has its own pipeline (proxy-based or shm-based).
        // Walk sub-pipeline in topological order.
        let sub_elements = walk_pipeline_topo(sub_pipeline);

        // Find bridge elements: trace from the sub-pipeline's source element
        // upstream into the source pipeline.
        let mut bridge = Vec::new();
        if let Some(first_sub_name) = sub_elements.first() {
            if let Some(src_el) = sub_pipeline.by_name(first_sub_name) {
                // Find the paired sink element in the source pipeline.
                // proxysrc↔proxysink communicate via a GObject property;
                // shmsrc↔shmsink communicate via a shared socket-path string.
                let paired_sink_el = find_paired_sink_element(&src_el, src_children);
                if let Some(paired_sink_el) = paired_sink_el {
                    // Walk upstream from the paired sink through the source pipeline
                    bridge = walk_upstream_to_tee(&paired_sink_el, src_children);
                }
            }
        }

        let mut elements = bridge;
        elements.extend(sub_elements);
        elements
    } else {
        // No sub-pipeline (webrtc, rtsp) — all elements are in the source pipeline.
        // These appear in topology branches, so return empty; the topology handles it.
        Vec::new()
    }
}

/// Find the paired sink element in the source pipeline for a sub-pipeline's source element.
///
/// Supports two bridge mechanisms:
/// - **proxy**: `proxysrc` has a `"proxysink"` GObject property pointing to its pair.
/// - **shm**: `shmsrc` and `shmsink` share the same `"socket-path"` string property.
fn find_paired_sink_element(
    src_el: &gst::Element,
    src_children: &[gst::Element],
) -> Option<gst::Element> {
    let factory_name = src_el
        .factory()
        .map(|f| f.name().to_string())
        .unwrap_or_default();

    match factory_name.as_str() {
        "proxysrc" => {
            // proxysrc has a "proxysink" property of type Element.
            src_el.property::<Option<gst::Element>>("proxysink")
        }
        "shmsrc" => {
            // shmsrc↔shmsink share the same socket-path.
            let socket_path: String = src_el.property("socket-path");
            src_children
                .iter()
                .find(|child| {
                    child
                        .factory()
                        .map(|f| f.name().as_str() == "shmsink")
                        .unwrap_or(false)
                        && child.property::<String>("socket-path") == socket_path
                })
                .cloned()
        }
        _ => None,
    }
}

/// Walk upstream from `start` through elements in `src_children` until reaching a tee.
/// Returns element names in source→sink order (e.g., queue → proxysink/shmsink).
fn walk_upstream_to_tee(start: &gst::Element, src_children: &[gst::Element]) -> Vec<String> {
    let mut current = start.clone();
    let mut chain = vec![current.name().to_string()];
    loop {
        let upstream = current.sink_pads().into_iter().find_map(|p| {
            p.peer().and_then(|pp| {
                pp.parent_element()
                    .filter(|pe| src_children.iter().any(|c| c == pe))
            })
        });
        match upstream {
            Some(up) => {
                let type_name = up
                    .factory()
                    .map(|f| f.name().to_string())
                    .unwrap_or_default();
                if type_name == "tee" {
                    break;
                }
                chain.push(up.name().to_string());
                current = up;
            }
            None => break,
        }
    }
    chain.reverse(); // source → sink order
    chain
}

/// Walk a pipeline/bin and return element names in topological order (source → sink).
fn walk_pipeline_topo(pipeline: &gst::Pipeline) -> Vec<String> {
    let children: Vec<gst::Element> = pipeline.children();
    if children.is_empty() {
        return Vec::new();
    }

    let mut has_upstream = std::collections::HashSet::new();
    for el in &children {
        for pad in el.sink_pads() {
            if let Some(peer) = pad.peer() {
                if let Some(peer_el) = peer.parent_element() {
                    if children.iter().any(|c| c == &peer_el) {
                        has_upstream.insert(el.name().to_string());
                    }
                }
            }
        }
    }

    let sources: Vec<&gst::Element> = children
        .iter()
        .filter(|e| !has_upstream.contains(&e.name().to_string()))
        .collect();

    let mut ordered = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    for src in sources {
        queue.push_back(src.clone());
    }
    while let Some(el) = queue.pop_front() {
        let name = el.name().to_string();
        if !visited.insert(name.clone()) {
            continue;
        }
        ordered.push(name);
        for pad in el.src_pads() {
            if let Some(peer) = pad.peer() {
                if let Some(peer_el) = peer.parent_element() {
                    if children.iter().any(|c| c == &peer_el) {
                        queue.push_back(peer_el);
                    }
                }
            }
        }
    }
    ordered
}
