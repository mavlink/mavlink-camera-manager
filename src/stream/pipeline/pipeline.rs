use std::{collections::HashMap, sync::Arc, thread};

use anyhow::{bail, Context, Result};

use gstreamer::prelude::*;

use tracing::*;

use crate::{
    stream::sink::sink::{Sink, SinkInterface},
    video::types::VideoSourceType,
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    fake_pipeline::FakePipeline, redirect_pipeline::RedirectPipeline, runner::PipelineRunner,
    v4l_pipeline::V4lPipeline,
};

#[derive(Debug)]
pub enum Pipeline {
    V4l(V4lPipeline),
    Fake(FakePipeline),
    Redirect(RedirectPipeline),
}

impl Pipeline {
    pub fn inner_state(&mut self) -> &mut PipelineState {
        match self {
            Pipeline::V4l(pipeline) => &mut pipeline.state,
            Pipeline::Fake(pipeline) => &mut pipeline.state,
            Pipeline::Redirect(pipeline) => &mut pipeline.state,
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
        self.inner_state().add_sink(sink)
    }

    #[allow(dead_code)] // This functions is reserved here for when we start dynamically add/remove Sinks
    #[instrument(level = "debug")]
    pub fn remove_sink(&mut self, sink_id: String) -> Result<()> {
        self.inner_state().remove_sink(sink_id)
    }
}

#[derive(Debug)]
pub struct PipelineState {
    pub pipeline_id: String,
    pub pipeline: Arc<gstreamer::Pipeline>,
    pub tee: Arc<gstreamer::Element>,
    pub sinks: HashMap<String, Sink>,
    pub pipeline_runner: PipelineRunner,
    _watcher_thread_handle: std::thread::JoinHandle<()>,
}

pub trait PipelineGstreamerInterface {
    fn build_pipeline(
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gstreamer::Pipeline>;

    fn is_running(&self) -> bool;
}

pub const PIPELINE_TEE_NAME: &str = "Tee";

impl PipelineState {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let pipeline = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(_) => FakePipeline::build_pipeline(video_and_stream_information),
            VideoSourceType::Local(_) => V4lPipeline::build_pipeline(video_and_stream_information),
            VideoSourceType::Redirect(_) => {
                RedirectPipeline::build_pipeline(video_and_stream_information)
            }
        }?;
        // let pipeline_id = video_and_stream_information.name.clone();
        let pipeline_id = "0".to_string();

        let tee = Arc::new(
            pipeline
                .by_name(PIPELINE_TEE_NAME)
                .context(format!("no element named {PIPELINE_TEE_NAME:#?}"))?,
        );

        let pipeline_runner = PipelineRunner::new(&pipeline, pipeline_id.clone());
        let mut killswitch_receiver = pipeline_runner.get_receiver();

        let pipeline = Arc::new(pipeline);

        Ok(Self {
            pipeline_id: pipeline_id.clone(),
            pipeline,
            tee,
            sinks: Default::default(),
            pipeline_runner,
            _watcher_thread_handle: thread::spawn(move || loop {
                // Here we end the stream if any error is received. This should end all sessions too.
                if let Ok(reason) = killswitch_receiver.try_recv() {
                    info!("Removing stream {pipeline_id}. Reason: {reason}");
                    if let Err(reason) = crate::stream::manager::remove_stream(pipeline_id.as_str())
                    {
                        warn!("Failed removing stream {pipeline_id}. Reason: {reason}");
                    } else {
                        break;
                    }
                }
                thread::sleep(std::time::Duration::from_secs(1));
            }),
        })
    }

    /// Links the sink pad from the given Sink to this Pipeline's Tee element
    #[instrument(level = "debug")]
    pub fn add_sink(&mut self, sink: Sink) -> Result<()> {
        let sink_id = &sink.get_id().clone();

        // Request a new src pad for the Tee
        let tee_src_pad = self
            .tee
            .request_pad_simple("src_%u")
            .context("Failed requesting src pad for Tee")?;
        let sink = sink.set_tee_src_pad(tee_src_pad);

        // Add the Sink element to the Pipeline
        let pipeline = &self.pipeline;
        let sink_element = sink.get_element();
        pipeline.add(sink_element.as_ref() as &gstreamer::Element)?;

        // Link the new Tee's src pad to the Sink's sink pad
        let sink_pad = sink.get_sink_pad();
        let tee_src_pad = sink.get_tee_src_pad().context("Tee src pad is None")?;
        if let Err(error) = tee_src_pad.link(sink_pad) {
            pipeline.remove(sink_element.as_ref() as &gstreamer::Element)?;
            bail!(error)
        }

        pipeline.debug_to_dot_file(
            gstreamer::DebugGraphDetails::all(),
            format!("sink-{}-before-playing", &sink.get_id()),
        );

        if let Err(error) = pipeline.set_state(gstreamer::State::Playing) {
            pipeline.remove(sink_element.as_ref() as &gstreamer::Element)?;
            tee_src_pad.unlink(sink_pad)?;
            bail!(error)
        }

        pipeline.debug_to_dot_file(
            gstreamer::DebugGraphDetails::all(),
            format!("sink-{sink_id}-playing"),
        );

        drop(sink_element);

        self.sinks.insert(sink_id.to_string(), sink);

        Ok(())
    }

    /// Unlinks the src pad from this Sink from the given sink pad of a Tee element
    ///
    /// Important notes about pad unlinking: [here](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline)
    #[instrument(level = "info")]
    pub fn remove_sink(&mut self, sink_id: String) -> Result<()> {
        let pipeline_id = &self.pipeline_id;
        let sink = self.sinks.remove(&sink_id).context(format!(
            "Failed to remove sink {sink_id} from Sinks of the Pipeline {pipeline_id}"
        ))?;

        let pipeline = &self.pipeline;
        pipeline.debug_to_dot_file(
            gstreamer::DebugGraphDetails::all(),
            format!("sink-{sink_id}-before-removing"),
        );

        let sink_pad = sink.get_sink_pad();
        let tee_src_pad = sink.get_tee_src_pad().context("Tee src pad is None")?;
        if let Err(error) = tee_src_pad.unlink(sink_pad) {
            bail!("Failed unlinking element's sink {sink_id} from from Pipeline {pipeline_id} Tee's source. Reason: {error:?}");
        }

        let tee = pipeline
            .by_name(PIPELINE_TEE_NAME)
            .context(format!("no element named {PIPELINE_TEE_NAME:#?}"))?;
        if let Err(error) = tee.remove_pad(tee_src_pad) {
            bail!("Failed removing Tee's source pad. Reason: {error:?}");
        }

        let sink_element = sink.get_element();
        if let Err(error) = sink_element.set_state(gstreamer::State::Null) {
            bail!("Failed to set Sink to NULL. Reason: {error:#?}")
        }
        if let Err(error) = pipeline.remove(sink_element.as_ref() as &gstreamer::Element) {
            bail!(
                "Failed removing Sink element {sink_id} from Pipeline {pipeline_id}. Reason: {error:?}"
            );
        }

        // Set pipeline state to NULL when there are no consumers to save CPU usage.
        if tee.src_pads().is_empty() {
            if let Err(error) = pipeline.set_state(gstreamer::State::Null) {
                bail!("Failed to change state of Pipeline {pipeline_id} to NULL. Reason: {error}");
            }
        }

        pipeline.debug_to_dot_file(
            gstreamer::DebugGraphDetails::all(),
            format!("{sink_id}-dropping-after"),
        );

        Ok(())
    }
}

impl Drop for PipelineState {
    #[instrument(level = "debug")]
    fn drop(&mut self) {
        if let Err(reason) = self.pipeline.set_state(gstreamer::State::Null) {
            warn!("Failed setting Pipeline to NULL. Reason: {reason:#?}");
        }

        let sink_ids = self.sinks.keys().cloned().collect::<Vec<String>>();
        sink_ids.iter().for_each(|sink_id| {
            if let Err(error) = self.remove_sink(sink_id.to_string()) {
                warn!("{error:#?}");
            }
        });
        self.sinks.clear();
    }
}
