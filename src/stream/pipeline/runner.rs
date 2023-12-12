use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tracing::*;

use crate::stream::gst::utils::wait_for_element_state_async;

#[derive(Debug)]
pub struct PipelineRunner {
    start: tokio::sync::mpsc::Sender<()>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for PipelineRunner {
    #[instrument(level = "debug", skip(self))]
    fn drop(&mut self) {
        debug!("Dropping PipelineRunner...");

        if let Some(handle) = self.handle.take() {
            if !handle.is_finished() {
                handle.abort();
                tokio::task::spawn(async move {
                    let _ = handle.await;
                    debug!("PipelineRunner task aborted");
                });
            } else {
                debug!("PipelineRunner task nicely finished!");
            }
        }

        debug!("PipelineRunner Dropped!");
    }
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

        let (start_tx, start_rx) = tokio::sync::mpsc::channel(1);

        debug!("Starting PipelineRunner task...");

        Ok(Self {
            start: start_tx,
            handle: Some(tokio::spawn(async move {
                debug!("PipelineRunner task started!");
                match Self::runner(pipeline_weak, pipeline_id, start_rx, allow_block).await {
                    Ok(_) => debug!("PipelineRunner task eneded with no errors"),
                    Err(error) => warn!("PipelineRunner task ended with error: {error:#?}"),
                };
            })),
        })
    }

    #[instrument(level = "debug", skip(self))]
    pub fn start(&self) -> Result<()> {
        let start = self.start.clone();
        tokio::spawn(async move {
            if let Err(error) = start.send(()).await {
                error!("Failed to send start command: {error:#?}");
            }
        });

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    pub fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
    }

    #[instrument(level = "debug", skip(pipeline_weak, pipeline_id, start))]
    async fn runner(
        pipeline_weak: gst::glib::WeakRef<gst::Pipeline>,
        pipeline_id: uuid::Uuid,
        mut start: tokio::sync::mpsc::Receiver<()>,
        allow_block: bool,
    ) -> Result<()> {
        let (finsh_tx, mut finish) = tokio::sync::mpsc::channel(1);
        let _bus_watcher = {
            let pipeline = pipeline_weak
                .upgrade()
                .context("Unable to access the Pipeline from its weak reference")?;

            let bus = pipeline
                .bus()
                .context("Unable to access the pipeline bus")?;

            /* Iterate messages on the bus until an error or EOS occurs,
             * although in this example the only error we'll hopefully
             * get is if the user closes the output window */
            let pipeline_weak = pipeline_weak.clone();
            bus.add_watch(move |_bus, message| {
                use gst::MessageView;

                let Some(pipeline) = pipeline_weak.upgrade() else {
                    return gst::glib::ControlFlow::Break;
                };

                match message.view() {
                    MessageView::Eos(eos) => {
                        pipeline.debug_to_dot_file_with_ts(
                            gst::DebugGraphDetails::all(),
                            format!("pipeline-{pipeline_id}-eos"),
                        );
                        let msg = format!("Received EndOfStream: {eos:?}");
                        let _ = finsh_tx.blocking_send(msg);
                        return gst::glib::ControlFlow::Break;
                    }
                    MessageView::Error(error) => {
                        let msg = format!(
                            "Error from {:?}: {} ({:?})",
                            error.src().map(|s| s.path_string()),
                            error.error(),
                            error.debug()
                        );
                        pipeline.debug_to_dot_file_with_ts(
                            gst::DebugGraphDetails::all(),
                            format!("pipeline-{pipeline_id}-error"),
                        );
                        let _ = finsh_tx.blocking_send(msg);
                        return gst::glib::ControlFlow::Break;
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
                }

                gst::glib::ControlFlow::Continue
            })?
        };

        // Wait until start receive the signal
        debug!("PipelineRunner waiting for start command...");
        loop {
            tokio::select! {
                reason = finish.recv() => {
                    return Err(anyhow!("{reason:?}"));
                }
                _ = start.recv() => {
                    debug!("PipelineRunner received start command");

                    let pipeline = pipeline_weak
                        .upgrade()
                        .context("Unable to access the Pipeline from its weak reference")?;

                    if pipeline.current_state() != gst::State::Playing {
                        if let Err(error) = pipeline.set_state(gst::State::Playing) {
                            error!(
                                "Failed setting Pipeline {} to Playing state. Reason: {:?}",
                                pipeline_id, error
                            );
                            continue;
                        }
                    }

                    if let Err(error) = wait_for_element_state_async(
                        pipeline_weak.clone(),
                        gst::State::Playing,
                        100,
                        5,
                    ).await {
                        return Err(anyhow!("{error:?}"));
                    }

                    break;
                }
            };
        }

        debug!("PipelineRunner started!");

        // Check if we need to break external loop.
        // Some cameras have a duplicated timestamp when starting.
        // to avoid restarting the camera once and once again,
        // this checks for a maximum number of lost before restarting.
        let mut previous_position: Option<gst::ClockTime> = None;
        let mut lost_timestamps: usize = 0;
        let max_lost_timestamps: usize = 30;

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            if let Some(reason) = finish.recv().await {
                return Err(anyhow!("{reason:?}"));
            }

            if !allow_block {
                // Restart pipeline if pipeline position do not change,
                // occur if usb connection is lost and gst do not detect it
                let pipeline = pipeline_weak
                    .upgrade()
                    .context("Unable to access the Pipeline from its weak reference")?;

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
        }
    }
}
