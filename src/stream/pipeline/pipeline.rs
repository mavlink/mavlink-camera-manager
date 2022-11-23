use std::{collections::HashMap, thread};

use anyhow::{anyhow, Context, Result};

use gst::prelude::*;

use tracing::*;

use crate::{
    stream::{
        manager::Manager,
        sink::sink::{Sink, SinkInterface},
        webrtc::signalling_server::StreamManagementInterface,
    },
    video::types::VideoSourceType,
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    fake_pipeline::FakePipeline, redirect_pipeline::RedirectPipeline, runner::PipelineRunner,
    v4l_pipeline::V4lPipeline,
};

#[enum_dispatch]
pub trait PipelineGstreamerInterface {
    fn is_running(&self) -> bool;
}

#[enum_dispatch(PipelineGstreamerInterface)]
#[derive(Debug)]
pub enum Pipeline {
    V4l(V4lPipeline),
    Fake(FakePipeline),
    Redirect(RedirectPipeline),
}

impl Pipeline {
    pub fn inner_state_mut(&mut self) -> &mut PipelineState {
        match self {
            Pipeline::V4l(pipeline) => &mut pipeline.state,
            Pipeline::Fake(pipeline) => &mut pipeline.state,
            Pipeline::Redirect(pipeline) => &mut pipeline.state,
        }
    }

    pub fn inner_state_as_ref(&self) -> &PipelineState {
        match self {
            Pipeline::V4l(pipeline) => &pipeline.state,
            Pipeline::Fake(pipeline) => &pipeline.state,
            Pipeline::Redirect(pipeline) => &pipeline.state,
        }
    }

    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline_state = PipelineState::try_new(video_and_stream_information)?;
        Ok(match &video_and_stream_information.video_source {
            VideoSourceType::Gst(_) => Pipeline::Fake(FakePipeline {
                state: pipeline_state,
            }),
            VideoSourceType::Local(_) => Pipeline::V4l(V4lPipeline {
                state: pipeline_state,
            }),
            VideoSourceType::Redirect(_) => Pipeline::Redirect(RedirectPipeline {
                state: pipeline_state,
            }),
        })
    }

    #[instrument(level = "debug")]
    pub fn add_sink(&mut self, sink: Sink) -> Result<()> {
        self.inner_state_mut().add_sink(sink)
    }

    #[allow(dead_code)] // This functions is reserved here for when we start dynamically add/remove Sinks
    #[instrument(level = "debug")]
    pub fn remove_sink(&mut self, sink_id: &uuid::Uuid) -> Result<()> {
        self.inner_state_mut().remove_sink(sink_id)
    }
}

#[derive(Debug)]
pub struct PipelineState {
    pub pipeline_id: uuid::Uuid,
    pub pipeline: gst::Pipeline,
    pub tee: gst::Element,
    pub sinks: HashMap<uuid::Uuid, Sink>,
    pub pipeline_runner: PipelineRunner,
    _watcher_thread_handle: std::thread::JoinHandle<()>,
}

pub const PIPELINE_TEE_NAME: &str = "Tee";

impl PipelineState {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(_) => FakePipeline::try_new(video_and_stream_information),
            VideoSourceType::Local(_) => V4lPipeline::try_new(video_and_stream_information),
            VideoSourceType::Redirect(_) => RedirectPipeline::try_new(video_and_stream_information),
        }?;
        let pipeline_id = Manager::generate_uuid();

        let tee = pipeline
            .by_name(PIPELINE_TEE_NAME)
            .context(format!("no element named {PIPELINE_TEE_NAME:#?}"))?;

        let pipeline_runner = PipelineRunner::new(&pipeline, pipeline_id.clone());
        let mut killswitch_receiver = pipeline_runner.get_receiver();

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-created"),
        );

        Ok(Self {
            pipeline_id: pipeline_id.clone(),
            pipeline,
            tee,
            sinks: Default::default(),
            pipeline_runner,
            _watcher_thread_handle: thread::spawn(move || loop {
                // Here we end the stream if any error is received. This should end all sessions too.
                if let Ok(reason) = killswitch_receiver.try_recv() {
                    warn!("KILLSWITCH RECEIVED AS {pipeline_id:#?}. Reason: {reason:#?}");
                    // TODO: We need to decide the behavior and implement it. The older behavior was to remove the entire pipeline whenever any error had occured, thus the "killswitch"
                    if let Err(reason) = Manager::remove_stream(&pipeline_id) {
                        warn!("Failed removing Pipeline {pipeline_id}. Reason: {reason}");
                    } else {
                        info!("Pipeline {pipeline_id} removed. Reason: {reason}");
                    }
                    break;
                }
                thread::sleep(std::time::Duration::from_secs(1));
            }),
        })
    }

    /// Links the sink pad from the given Sink to this Pipeline's Tee element
    #[instrument(level = "debug")]
    pub fn add_sink(&mut self, mut sink: Sink) -> Result<()> {
        let pipeline_id = &self.pipeline_id;

        // Request a new src pad for the Tee
        let tee_src_pad = self.tee.request_pad_simple("src_%u").context(format!(
            "Failed requesting src pad for Tee of the pipeline {pipeline_id}"
        ))?;
        debug!("Got tee's src pad {:#?}", tee_src_pad.name());

        let pipeline = &self.pipeline;

        // Temporarely set to Ready to avoid losing frames during connection of the new Sink
        // if let Err(error) = pipeline.set_state(gst::State::Ready) {
        //     sink.unlink(pipeline, &self.pipeline_id)?;
        //     bail!(error)
        // }

        // Link the Sink
        sink.link(pipeline, &self.pipeline_id, tee_src_pad)?;
        let sink_id = &sink.get_id();

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-before-playing"),
        );

        // Start it
        if let Err(error) = pipeline.set_state(gst::State::Playing) {
            sink.unlink(pipeline, &self.pipeline_id)?;
            bail!(error)
        }

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-playing"),
        );

        self.sinks.insert(sink_id.clone(), sink);

        Ok(())
    }

    /// Unlinks the src pad from this Sink from the given sink pad of a Tee element
    ///
    /// Important notes about pad unlinking: [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    #[instrument(level = "info")]
    pub fn remove_sink(&mut self, sink_id: &uuid::Uuid) -> Result<()> {
        let pipeline_id = &self.pipeline_id;
        let sink = self.sinks.remove(sink_id).context(format!(
            "Failed to remove sink {sink_id} from Sinks of the Pipeline {pipeline_id}"
        ))?;

        let pipeline = &self.pipeline;
        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-before-removing"),
        );

        // Unlink the Sink
        sink.unlink(pipeline, pipeline_id)?;

        // Set pipeline state to NULL when there are no consumers to save CPU usage.
        let tee = pipeline
            .by_name(PIPELINE_TEE_NAME)
            .context(format!("no element named {PIPELINE_TEE_NAME:#?}"))?;
        if tee.src_pads().is_empty() {
            if let Err(error) = pipeline.set_state(gst::State::Null) {
                return Err(anyhow!(
                    "Failed to change state of Pipeline {pipeline_id} to NULL. Reason: {error}"
                ));
            }
        }

        pipeline.debug_to_dot_file_with_ts(
            gst::DebugGraphDetails::all(),
            format!("pipeline-{pipeline_id}-sink-{sink_id}-after-removing"),
        );

        Ok(())
    }
}

impl Drop for PipelineState {
    #[instrument(level = "debug")]
    fn drop(&mut self) {
        // if let Err(reason) = self.pipeline.set_state(gst::State::Null) {
        //     warn!("Failed setting Pipeline to NULL. Reason: {reason:#?}");
        // }

        // let sink_ids = self.sinks.keys().cloned().collect::<Vec<uuid::Uuid>>();
        // sink_ids.iter().for_each(|sink_id| {
        //     if let Err(error) = self.remove_sink(sink_id) {
        //         warn!("{error:#?}");
        //     }
        // });
        self.sinks.clear();
    }
}
