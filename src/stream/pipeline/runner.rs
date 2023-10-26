use std::sync::{Arc, Mutex};

use gst::prelude::*;

use anyhow::{anyhow, Context, Result};

use tracing::*;

use crate::stream::gst::utils::wait_for_element_state;

#[derive(Debug)]
#[allow(dead_code)]
pub struct PipelineRunner {
    pipeline_weak: gst::glib::WeakRef<gst::Pipeline>,
    start: Arc<Mutex<bool>>,
    watcher_thread_handle: std::thread::JoinHandle<()>,
}

impl PipelineRunner {
    #[instrument(level = "debug")]
    pub fn try_new(
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        allow_block: bool,
    ) -> Result<Self> {
        let pipeline_weak = pipeline.downgrade();
        let pipeline_id = *pipeline_id;

        let start = Arc::new(Mutex::new(false));

        Ok(Self {
            pipeline_weak: pipeline_weak.clone(),
            start: start.clone(),
            watcher_thread_handle: std::thread::Builder::new()
                .name(format!("PipelineRunner-{pipeline_id}"))
                .spawn(move || {
                    if let Err(error) =
                        PipelineRunner::runner(pipeline_weak, &pipeline_id, start, allow_block)
                    {
                        error!("PipelineWatcher ended with error: {error}");
                    } else {
                        info!("PipelineWatcher ended normally.");
                    }
                })
                .context(format!(
                    "Failed when spawing PipelineRunner thread for Pipeline {pipeline_id:#?}"
                ))?,
        })
    }

    #[instrument(level = "debug", skip(self))]
    pub fn start(&self) -> Result<()> {
        *self.start.lock().unwrap() = true;
        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    pub fn is_running(&self) -> bool {
        !self.watcher_thread_handle.is_finished()
    }

    #[instrument(level = "debug")]
    fn runner(
        pipeline_weak: gst::glib::WeakRef<gst::Pipeline>,
        pipeline_id: &uuid::Uuid,
        start: Arc<Mutex<bool>>,
        allow_block: bool,
    ) -> Result<()> {
        let pipeline = pipeline_weak
            .upgrade()
            .context("Unable to access the Pipeline from its weak reference")?;

        let bus = pipeline
            .bus()
            .context("Unable to access the pipeline bus")?;

        // Check if we need to break external loop.
        // Some cameras have a duplicated timestamp when starting.
        // to avoid restarting the camera once and once again,
        // this checks for a maximum number of lost before restarting.
        let mut previous_position: Option<gst::ClockTime> = None;
        let mut lost_timestamps: usize = 0;
        let max_lost_timestamps: usize = 30;

        'outer: loop {
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Wait the signal to start
            if *start.lock().unwrap() && pipeline.current_state() != gst::State::Playing {
                if let Err(error) = pipeline.set_state(gst::State::Playing) {
                    return Err(anyhow!(
                        "Failed setting Pipeline {pipeline_id} to Playing state. Reason: {error:?}"
                    ));
                }
                if let Err(error) = wait_for_element_state(
                    pipeline.upcast_ref::<gst::Element>(),
                    gst::State::Playing,
                    100,
                    5,
                ) {
                    return Err(anyhow!(
                        "Failed setting Pipeline {pipeline_id} to Playing state. Reason: {error:?}"
                    ));
                }
            }

            'inner: loop {
                // Restart pipeline if pipeline position do not change,
                // occur if usb connection is lost and gst do not detect it
                if !allow_block {
                    if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                        previous_position = match previous_position {
                            Some(current_previous_position) => {
                                if current_previous_position.nseconds() != 0
                                    && current_previous_position == position
                                {
                                    lost_timestamps += 1;
                                } else if lost_timestamps > 0 {
                                    // We are back in track, erase lost timestamps
                                    warn!("Position normalized, but didn't changed for {lost_timestamps} timestamps");
                                    lost_timestamps = 0;
                                }
                                if lost_timestamps == 1 {
                                    warn!("Position did not change for {lost_timestamps}, silently tracking until {max_lost_timestamps}, then the stream will be recreated");
                                } else if lost_timestamps > max_lost_timestamps {
                                    return Err(anyhow!("Pipeline lost too many timestamps (max. was {max_lost_timestamps})"));
                                }

                                Some(position)
                            }
                            None => Some(position),
                        }
                    }
                }

                /* Iterate messages on the bus until an error or EOS occurs,
                 * although in this example the only error we'll hopefully
                 * get is if the user closes the output window */
                while let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
                    use gst::MessageView;

                    match msg.view() {
                        MessageView::Eos(eos) => {
                            debug!("Received EndOfStream: {eos:?}");
                            pipeline.debug_to_dot_file_with_ts(
                                gst::DebugGraphDetails::all(),
                                format!("pipeline-{pipeline_id}-eos"),
                            );
                            break 'outer;
                        }
                        MessageView::Error(error) => {
                            error!(
                                "Error from {:?}: {} ({:?})",
                                error.src().map(|s| s.path_string()),
                                error.error(),
                                error.debug()
                            );
                            pipeline.debug_to_dot_file_with_ts(
                                gst::DebugGraphDetails::all(),
                                format!("pipeline-{pipeline_id}-error"),
                            );
                            break 'inner;
                        }
                        MessageView::StateChanged(state) => {
                            pipeline.debug_to_dot_file_with_ts(
                                gst::DebugGraphDetails::all(),
                                format!(
                                    "pipeline-{pipeline_id}-{:?}-to-{:?}",
                                    state.old(),
                                    state.current()
                                ),
                            );

                            trace!(
                                "State changed from {:?}: {:?} to {:?} ({:?})",
                                state.src().map(|s| s.path_string()),
                                state.old(),
                                state.current(),
                                state.pending()
                            );
                        }
                        MessageView::Latency(latency) => {
                            let current_latency = pipeline.latency();
                            trace!("Latency message: {latency:?}. Current latency: {latency:?}",);
                            if let Err(error) = pipeline.recalculate_latency() {
                                warn!("Failed to recalculate latency: {error:?}");
                            }
                            let new_latency = pipeline.latency();
                            if current_latency != new_latency {
                                debug!("New latency: {new_latency:?}");
                            }
                        }
                        other_message => trace!("{other_message:#?}"),
                    };
                }
            }
        }

        Ok(())
    }
}
