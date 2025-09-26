use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::gst::utils::wait_for_element_state_async,
    video_stream::types::VideoAndStreamInformation,
};

#[derive(Debug)]
pub struct PipelineRunner {
    start: tokio::sync::mpsc::Sender<()>,
    handle: Option<tokio::task::JoinHandle<()>>,
    pipeline_id: Arc<uuid::Uuid>,
}

impl Drop for PipelineRunner {
    #[instrument(level = "debug", skip(self), fields(pipeline_id = self.pipeline_id.to_string()))]
    fn drop(&mut self) {
        debug!("Dropping PipelineRunner...");

        if let Some(handle) = self.handle.take() {
            if !handle.is_finished() {
                handle.abort();
                tokio::spawn(async move {
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
    #[instrument(level = "debug", skip(pipeline))]
    pub fn try_new(
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        allow_block: bool,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self> {
        let pipeline_weak = pipeline.downgrade();

        let (start_tx, start_rx) = tokio::sync::mpsc::channel(1);

        debug!("Starting PipelineRunner task...");

        let span = span!(
            Level::DEBUG,
            "PipelineRunner task",
            id = pipeline_id.to_string()
        );
        let task_handle = tokio::spawn({
            let video_and_stream_information = video_and_stream_information.clone();
            let pipeline_id = pipeline_id.clone();
            async move {
                debug!("task started!");
                match Self::runner(
                    pipeline_weak,
                    pipeline_id,
                    start_rx,
                    allow_block,
                    &video_and_stream_information,
                )
                .await
                {
                    Ok(_) => debug!("task ended with no errors"),
                    Err(error) => warn!("task ended with error: {error:#?}"),
                };
            }
            .instrument(span)
        });

        Ok(Self {
            start: start_tx,
            handle: Some(task_handle),
            pipeline_id: pipeline_id.clone(),
        })
    }

    #[instrument(level = "debug", skip(self), fields(pipeline_id = self.pipeline_id.to_string()))]
    pub fn start(&self) -> Result<()> {
        let start = self.start.clone();
        tokio::spawn(async move {
            debug!("Pipeline Start task started!");
            if let Err(error) = start.send(()).await {
                error!("Failed to send start command: {error:#?}");
            }
            debug!("Pipeline Start task ended");
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

    #[instrument(
        level = "debug",
        skip(pipeline_weak, start, video_and_stream_information)
    )]
    async fn runner(
        pipeline_weak: gst::glib::WeakRef<gst::Pipeline>,
        pipeline_id: Arc<uuid::Uuid>,
        mut start: tokio::sync::mpsc::Receiver<()>,
        allow_block: bool,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<()> {
        let (finish_tx, mut finish) = tokio::sync::mpsc::channel(1);

        let pipeline = pipeline_weak
            .upgrade()
            .context("Unable to access the Pipeline from its weak reference")?;

        let pipeline_name = pipeline.name().to_string();

        let (bus_tx, bus_rx) = tokio::sync::mpsc::unbounded_channel::<gst::Message>();
        let bus = pipeline
            .bus()
            .context("Unable to access the pipeline bus")?;
        bus.set_sync_handler({
            let pipeline_name = pipeline_name.clone();

            move |_, msg| {
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                if let gst::MessageView::StreamStatus(status) = msg.view() {
                    let (status_type, element) = status.get();
                    if matches!(status_type, gst::StreamStatusType::Enter) {
                        if let Err(error) = thread_priority::set_thread_priority_and_policy(
                            thread_priority::thread_native_id(),
                            thread_priority::ThreadPriority::Max,
                            thread_priority::ThreadSchedulePolicy::Realtime(
                                thread_priority::RealtimeThreadSchedulePolicy::RoundRobin,
                            ),
                        ) {
                            warn!("Failed configuring GStreamer stream thread: {error:}.");
                        } else {
                            let priority = thread_priority::get_current_thread_priority();
                            let scheduler = thread_priority::get_thread_scheduling_attributes();
                            info!("GStreamer stream thread sucessfully configured to MAX priority ({priority:?}) and real-time round robyn ({scheduler:?}). From element {:?}, from Pipeline {pipeline_name:?}", element.name());
                        }
                    }
                }

                let _ = bus_tx.send(msg.to_owned());
                gst::BusSyncReply::Drop
            }
        });

        /* Iterate messages on the bus until an error or EOS occurs,
         * although in this example the only error we'll hopefully
         * get is if the user closes the output window */
        debug!("Starting BusWatcher task for Pipeline {pipeline_name:?}...");
        tokio::spawn(bus_watcher_task(
            pipeline_weak.clone(),
            pipeline_id.clone(),
            bus_rx,
            finish_tx,
        ));

        // Wait until start receive the signal
        debug!("PipelineRunner waiting for start commandk for Pipeline {pipeline_name:?}...");
        loop {
            tokio::select! {
                reason = finish.recv() => {
                    return Err(anyhow!("{reason:?}"));
                }
                start_cmd = start.recv() => {
                    match start_cmd {
                        Some(()) => {
                            debug!("PipelineRunner received start commandk for Pipeline {pipeline_name:?}");

                            let pipeline = pipeline_weak
                                .upgrade()
                                .context("Unable to access the Pipeline ({pipeline_name:?}) from its weak reference")?;

                            if pipeline.current_state() != gst::State::Playing {
                                if let Err(error) = pipeline.set_state(gst::State::Playing) {
                                    error!(
                                        "Failed setting Pipeline {pipeline_name:?} to Playing state. Reason: {error:?}"
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
                        None => {
                            return Err(anyhow!("start channel closed before sending command from Pipeline {pipeline_name:?}"));
                        }
                    }

                }
            };
        }

        info!("PipelineRunner started for Pipeline {pipeline_name:?}!");

        let frame_duration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            crate::stream::types::CaptureConfiguration::Video(video_capture_configuration) => {
                let frame_interval = &video_capture_configuration.frame_interval;

                if frame_interval.denominator > 0 && frame_interval.numerator > 0 {
                    std::time::Duration::from_secs_f64(
                        frame_interval.numerator as f64 / frame_interval.denominator as f64,
                    )
                } else {
                    warn!("Invalid frame_interval {frame_interval:?}, using fallback of 1 FPS (Pipeline {pipeline_name:?})");
                    std::time::Duration::from_secs(1)
                }
            }
            crate::stream::types::CaptureConfiguration::Redirect(_) => {
                return Err(anyhow!(
                    "PipelineRunner aborted for Pipeline {pipeline_name:?}: Redirect CaptureConfiguration means the stream was not initialized yet"
                ));
            }
        };

        // Check if we need to break external loop.
        // Some cameras have a duplicated timestamp when starting.
        // to avoid restarting the camera once and once again,
        // this checks for a maximum number of lost before restarting.
        let mut previous_position: Option<gst::ClockTime> = None;
        let mut lost_ticks: usize = 0;
        let max_lost_ticks: usize = 30;
        let min_lost_ticks_before_considering_stuck = 3;

        let mut period = tokio::time::interval(frame_duration);

        loop {
            tokio::select! {
                reason = finish.recv() => {
                    return Err(anyhow!("{reason:?}"));
                }
                _ = period.tick() => {
                    if !allow_block {
                        // Restart pipeline if pipeline position do not change,
                        // occur if usb connection is lost and gst do not detect it
                        let pipeline = pipeline_weak
                            .upgrade()
                            .context("Unable to access the Pipeline {pipeline_name:?} from its weak reference")?;



                        if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                            match previous_position {
                                Some(prev_pos) => {
                                    if prev_pos == position && prev_pos != gst::ClockTime::ZERO {
                                        lost_ticks += 1;

                                        if lost_ticks == min_lost_ticks_before_considering_stuck {
                                            warn!("Position unchanged for {min_lost_ticks_before_considering_stuck} consecutive ticks. Pipeline {pipeline_name:?} may be stuck.")
                                        } else if lost_ticks > max_lost_ticks {
                                            error!("Pipeline {pipeline_name:?} lost too many timestamps ({lost_ticks} > max {max_lost_ticks}). Last position: {position:?}");
                                            return Err(anyhow!("Pipeline {pipeline_name:?} appears stuck — position unchanged for too long"));
                                        }
                                    } else {

                                        let delta_ns = position.nseconds().saturating_sub(prev_pos.nseconds());
                                        let delta_ms = delta_ns as f64 / 1_000_000.0;

                                        if delta_ns > 1_000_000 || lost_ticks >= min_lost_ticks_before_considering_stuck {
                                            trace!("Position advanced by {delta_ms:.2}ms ({prev_pos} -> {position}) for Pipeline {pipeline_name:?} ")
                                        }

                                        // We are back in track, erase lost timestamps
                                        if delta_ns > 1_000_000 {
                                            if lost_ticks >= min_lost_ticks_before_considering_stuck {
                                                warn!("Position normalized for Pipeline {pipeline_name:?}: advanced by {delta_ms:.2}ms after {lost_ticks} lost ticks");
                                            }
                                            lost_ticks = 0;
                                        }
                                    }
                                }
                                None => {
                                    debug!("First position recorded for Pipeline {pipeline_name:?}: {position:?}");
                                }
                            }

                            previous_position = Some(position);

                        } else {
                            debug!("Failed to query position for Pipeline {pipeline_name:?}");
                        }
                    }
                }
            }
        }
    }
}

#[instrument(level = "debug", skip(pipeline_weak, bus_rx, finish_tx))]
async fn bus_watcher_task(
    pipeline_weak: gst::glib::WeakRef<gst::Pipeline>,
    pipeline_id: Arc<uuid::Uuid>,
    mut bus_rx: tokio::sync::mpsc::UnboundedReceiver<gst::Message>,
    finish_tx: tokio::sync::mpsc::Sender<String>,
) {
    debug!("BusWatcher task started!");

    while let Some(message) = bus_rx.recv().await {
        use gst::MessageView;

        let Some(pipeline) = pipeline_weak.upgrade() else {
            break;
        };

        let pipeline_name = pipeline.name();

        match message.view() {
            MessageView::Eos(eos) => {
                pipeline.debug_to_dot_file_with_ts(
                    gst::DebugGraphDetails::all(),
                    format!("pipeline-{pipeline_id}-eos"),
                );
                let msg = format!("Received EndOfStream: {eos:?}");
                trace!(msg);
                let _ = finish_tx.send(msg).await;
                break;
            }
            MessageView::Error(error) => {
                let msg = format!(
                    "Error from {:?} for Pipeline {pipeline_name:?}: {} ({:?})",
                    error.src().map(|s| s.path_string()),
                    error.error(),
                    error.debug()
                );
                pipeline.debug_to_dot_file_with_ts(
                    gst::DebugGraphDetails::all(),
                    format!("pipeline-{pipeline_id}-error"),
                );
                trace!(msg);
                let _ = finish_tx.send(msg).await;
                break;
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
                    "Pipeline {pipeline_name:?} State changed from {:?}: {:?} to {:?} ({:?})",
                    state.src().map(|s| s.path_string()),
                    state.old(),
                    state.current(),
                    state.pending()
                );
            }
            MessageView::Latency(latency) => {
                let current_latency = pipeline.latency();
                trace!("Latency message: {latency:?}. Current latency: {current_latency:?}",);
                if let Err(error) = pipeline.recalculate_latency() {
                    warn!(
                        "Failed to recalculate latency for Pipeline {pipeline_name:?}: {error:?}"
                    );
                }
                let new_latency = pipeline.latency();
                if current_latency != new_latency {
                    debug!("New latency for Pipeline {pipeline_name:?}: {new_latency:?}");
                }
            }
            other_message => trace!("{other_message:#?}"),
        }
    }

    debug!("BusWatcher task ended!");
}
