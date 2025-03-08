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

use crate::{
    stream::{
        gst::utils::wait_for_element_state,
        rtsp::rtsp_server::RTSPServer,
        sink::{Sink, SinkInterface},
    },
    video::types::VideoSourceType,
    video_stream::types::VideoAndStreamInformation,
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

    #[instrument(level = "debug")]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        pipeline_id: &Arc<uuid::Uuid>,
    ) -> Result<Self> {
        let pipeline_state: PipelineState =
            PipelineState::try_new(video_and_stream_information, pipeline_id)?;
        Ok(match &video_and_stream_information.video_source {
            VideoSourceType::Gst(video_source_gst) => match video_source_gst.source {
                crate::video::video_source_gst::VideoSourceGstType::Local(_) => todo!(),
                crate::video::video_source_gst::VideoSourceGstType::Fake(_) => {
                    Pipeline::Fake(FakePipeline {
                        state: pipeline_state,
                    })
                }
                crate::video::video_source_gst::VideoSourceGstType::QR(_) => {
                    Pipeline::QR(QrPipeline {
                        state: pipeline_state,
                    })
                }
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

    #[instrument(level = "debug", skip(self))]
    pub fn add_sink(&mut self, sink: Sink) -> Result<()> {
        self.inner_state_mut().add_sink(sink)
    }

    #[allow(dead_code)] // This functions is reserved here for when we start dynamically add/remove Sinks
    #[instrument(level = "debug", skip(self))]
    pub fn remove_sink(&mut self, sink_id: &uuid::Uuid) -> Result<()> {
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
    #[instrument(level = "debug")]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        pipeline_id: &Arc<uuid::Uuid>,
    ) -> Result<Self> {
        let pipeline = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(video) => match video.source {
                crate::video::video_source_gst::VideoSourceGstType::Local(_) => todo!(),
                crate::video::video_source_gst::VideoSourceGstType::Fake(_) => {
                    FakePipeline::try_new(pipeline_id, video_and_stream_information)
                }
                crate::video::video_source_gst::VideoSourceGstType::QR(_) => {
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

        let pipeline_runner = PipelineRunner::try_new(&pipeline, pipeline_id, false)?;

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
    #[instrument(level = "debug", skip(self))]
    pub fn add_sink(&mut self, mut sink: Sink) -> Result<()> {
        let pipeline_id = &self.pipeline_id;

        // Request a new src pad for the used Tee
        // Note: Here we choose if the sink will receive a Video or RTP packages
        let tee = match sink {
            Sink::Image(_) => &self.video_tee,
            Sink::Udp(_) | Sink::Rtsp(_) | Sink::WebRTC(_) => &self.rtp_tee,
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

        if let Err(error) = wait_for_element_state(
            gst::prelude::ObjectExt::downgrade(pipeline),
            gst::State::Playing,
            100,
            2,
        ) {
            let _ = pipeline.set_state(gst::State::Null);
            sink.unlink(pipeline, pipeline_id)?;
            return Err(anyhow!(
                "Failed setting Pipeline {pipeline_id} to Playing state. Reason: {error:?}"
            ));
        }

        if let Sink::Rtsp(sink) = &sink {
            if let Some(rtp_tee) = &self.rtp_tee {
                let caps = &rtp_tee
                    .static_pad("sink")
                    .expect("No static sink pad found on capsfilter")
                    .current_caps()
                    .context("Failed to get caps from capsfilter sink pad")?;

                debug!("caps: {:#?}", caps.to_string());

                // In case it exisits, try to remove it first, but skip the result
                let _ = RTSPServer::stop_pipeline(&sink.path());

                RTSPServer::add_pipeline(&sink.scheme(), &sink.path(), &sink.socket_path(), caps)?;

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

        Ok(())
    }

    /// Unlinks the src pad from this Sink from the given sink pad of a Tee element
    ///
    /// Important notes about pad unlinking: [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    #[instrument(level = "info", skip(self))]
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

        // Set pipeline state to NULL when there are no consumers to save CPU usage.
        // TODO: We are skipping rtspsrc here because once back to null, we are having
        // trouble knowing how to propper reuse it.
        if !self
            .pipeline
            .children()
            .iter()
            .any(|child| child.name().starts_with("rtspsrc"))
        {
            if let Some(rtp_tee) = &self.rtp_tee {
                if rtp_tee.src_pads().is_empty() {
                    if let Err(error) = pipeline.set_state(gst::State::Null) {
                        return Err(anyhow!(
                            "Failed to change state of Pipeline {pipeline_id} to NULL. Reason: {error}"
                        ));
                    }
                }
            }
        }

        if let Sink::Rtsp(sink) = &sink {
            RTSPServer::stop_pipeline(&sink.path())?;
        }

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-after-removing"),
        );

        Ok(())
    }
}
