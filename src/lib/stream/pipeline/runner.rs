use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tracing::*;

use mcm_api::v1::{stats::StatsLevel, stream::VideoAndStreamInformation};

use crate::stream::stats::pipeline_analysis::{PipelineTopology, TopologyEdge, TopologyNode};

use crate::stream::{gst::utils::wait_for_element_state_async, stats::pipeline_analysis};

/// In-memory pipeline analysis: level from --pipeline-analysis-level or MCM_PIPELINE_ANALYSIS_LEVEL env.
/// "off" means disabled, "lite" or "full" means enabled with the corresponding stats level.
static ANALYSIS_LEVEL: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(crate::cli::manager::pipeline_analysis_level);
static ANALYSIS_ENABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| *ANALYSIS_LEVEL != "off");

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
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        stream_id: &Arc<uuid::Uuid>,
        allow_block: bool,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self> {
        Self::try_new_inner(
            pipeline,
            pipeline_id,
            stream_id,
            allow_block,
            true,
            video_and_stream_information,
        )
    }

    /// Like `try_new`, but with `realtime_threads = false`: streaming threads
    /// are set to background priority (`SCHED_OTHER`, nice 19) instead of
    /// `SCHED_RR` realtime. Use for auxiliary pipelines (e.g. thumbnail
    /// generation) that must not preempt video stream threads.
    pub fn try_new_background(
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        stream_id: &Arc<uuid::Uuid>,
        allow_block: bool,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self> {
        Self::try_new_inner(
            pipeline,
            pipeline_id,
            stream_id,
            allow_block,
            false,
            video_and_stream_information,
        )
    }

    fn try_new_inner(
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        stream_id: &Arc<uuid::Uuid>,
        allow_block: bool,
        realtime_threads: bool,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self> {
        let pipeline_weak = pipeline.downgrade();

        let (start_tx, start_rx) = tokio::sync::mpsc::channel(1);

        debug!("Starting PipelineRunner task...");

        let span = span!(Level::DEBUG, "PipelineRunner task");
        let task_handle = tokio::spawn({
            let video_and_stream_information = video_and_stream_information.clone();
            let pipeline_id = pipeline_id.clone();
            let stream_id = stream_id.clone();
            async move {
                debug!("task started!");
                match Self::runner(
                    pipeline_weak,
                    pipeline_id,
                    stream_id,
                    start_rx,
                    allow_block,
                    realtime_threads,
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
        skip(
            pipeline_weak,
            pipeline_id,
            stream_id,
            start,
            video_and_stream_information
        ),
        fields(realtime_threads)
    )]
    async fn runner(
        pipeline_weak: gst::glib::WeakRef<gst::Pipeline>,
        pipeline_id: Arc<uuid::Uuid>,
        stream_id: Arc<uuid::Uuid>,
        mut start: tokio::sync::mpsc::Receiver<()>,
        allow_block: bool,
        realtime_threads: bool,
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
                        if realtime_threads {
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
                        } else {
                            crate::helper::threads::lower_to_background_priority();
                            debug!(
                                "GStreamer stream thread set to background priority. \
                                 From element {:?}, from Pipeline {pipeline_name:?}",
                                element.name()
                            );
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
            mcm_api::v1::stream::CaptureConfiguration::Video(video_capture_configuration) => {
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
            mcm_api::v1::stream::CaptureConfiguration::Redirect(_) => {
                return Err(anyhow!(
                    "PipelineRunner aborted for Pipeline {pipeline_name:?}: Redirect CaptureConfiguration means the stream was not initialized yet"
                ));
            }
        };

        // ── Pipeline Analysis: generalized per-element instrumentation ──
        // Set the global stats level and window size from CLI on first pipeline creation only,
        // so that later API changes are not overwritten on pipeline restarts.
        static INIT_ANALYSIS_DEFAULTS: std::sync::Once = std::sync::Once::new();
        INIT_ANALYSIS_DEFAULTS.call_once(|| {
            match ANALYSIS_LEVEL.as_str() {
                "full" => pipeline_analysis::set_global_stats_level(StatsLevel::Full),
                "lite" => pipeline_analysis::set_global_stats_level(StatsLevel::Lite),
                _ => {} // "off" — leave default (Lite, but we won't create analyses)
            }
            pipeline_analysis::set_global_window_size(
                crate::cli::manager::pipeline_analysis_window_size(),
            );
        });

        let _pa = if *ANALYSIS_ENABLED {
            let pipeline = pipeline_weak
                .upgrade()
                .context("Unable to access Pipeline for analysis probes")?;

            let pa = Arc::new(pipeline_analysis::PipelineAnalysis::new(
                pipeline_name.clone(),
                stream_id.to_string(),
                frame_duration.as_secs_f64() * 1000.0,
            ));
            pa.install_probes(&pipeline);
            pipeline_analysis::register(&pa);
            info!(
                "Pipeline analysis enabled (level={}) for Pipeline {pipeline_name:?}",
                *ANALYSIS_LEVEL
            );
            pipeline_analysis::ensure_global_system_sampler_started();

            Some(pa)
        } else {
            None
        };

        // ── Tick-based position monitoring loop ──
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

                        if pipeline.current_state() != gst::State::Playing {
                            previous_position = None;
                            lost_ticks = 0;
                            continue;
                        }

                        if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                            match previous_position {
                                Some(prev_pos) => {
                                    if prev_pos == position && prev_pos != gst::ClockTime::ZERO {
                                        lost_ticks += 1;

                                        if lost_ticks == min_lost_ticks_before_considering_stuck {
                                            warn!("Position unchanged for {min_lost_ticks_before_considering_stuck} consecutive ticks. Pipeline {pipeline_name:?} may be stuck.");
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
                            trace!("Failed to query position for Pipeline {pipeline_name:?}");
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
    let Some(pipeline) = pipeline_weak.upgrade() else {
        return;
    };

    let pipeline_name = pipeline.name();

    debug!("BusWatcher task started for Pipeline {pipeline_name:?}!");

    while let Some(message) = bus_rx.recv().await {
        use gst::MessageView;

        let Some(pipeline) = pipeline_weak.upgrade() else {
            break;
        };

        match message.view() {
            MessageView::Eos(eos) => {
                pipeline.debug_to_dot_file_with_ts(
                    gst::DebugGraphDetails::all(),
                    format!("pipeline-{pipeline_id}-eos"),
                );

                let msg = format!("Received EndOfStream: {eos:?} for Pipeline {pipeline_name:?}");

                debug!(msg);
                let _ = finish_tx.send(msg).await;
                break;
            }
            MessageView::Error(error) => {
                pipeline.debug_to_dot_file_with_ts(
                    gst::DebugGraphDetails::all(),
                    format!("pipeline-{pipeline_id}-error"),
                );

                let msg = format!(
                    "Error from {:?} for Pipeline {pipeline_name:?}: {} ({:?})",
                    error.src().map(|s| s.path_string()),
                    error.error(),
                    error.debug()
                );

                debug!(msg);
                let _ = finish_tx.send(msg).await;
                break;
            }
            MessageView::StateChanged(state) => {
                let current = state.current();
                let previous = state.old();

                if current != previous {
                    pipeline.debug_to_dot_file_with_ts(
                        gst::DebugGraphDetails::all(),
                        format!("pipeline-{pipeline_id}-{previous:?}-to-{current:?}"),
                    );

                    trace!(
                        "Pipeline {pipeline_name:?} State changed from {:?}: {previous:?} to {current:?} ({:?})",
                        state.src().map(|s| s.path_string()),
                        state.pending()
                    );

                    if current == gst::State::Playing
                        && state
                            .src()
                            .is_some_and(|s| s.downcast_ref::<gst::Pipeline>().is_some())
                    {
                        debug!("Pipeline {pipeline_name:?} reached PLAYING state");
                    }
                }
            }
            MessageView::Latency(latency) => {
                let source_name = latency
                    .src()
                    .map(|s| s.path_string().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                debug!(
                    "Latency message received from {source_name} for Pipeline {pipeline_name:?}"
                );

                let current_latency = pipeline.latency();

                if let Some(time) = current_latency {
                    let latency_ms = time.nseconds() as f64 / 1_000_000.0;

                    if latency_ms > 100.0 {
                        warn!("High latency detected for Pipeline {pipeline_name:?}: {latency_ms:.2}ms - may cause noticeable delay");
                    } else {
                        debug!("Current Pipeline ({pipeline_name:?}) latency: {latency_ms:.2}ms");
                    }
                }

                // Recalculate latency to ensure it's up to date
                match pipeline.recalculate_latency() {
                    Ok(_) => {
                        let new_latency = pipeline.latency();
                        match (current_latency, new_latency) {
                            (Some(old), Some(new)) if old != new => {
                                let old_ms = old.nseconds() as f64 / 1_000_000.0;
                                let new_ms = new.nseconds() as f64 / 1_000_000.0;
                                debug!(
                                    "Latency updated for Pipeline {pipeline_name:?}: {old_ms:.2}ms -> {new_ms:.2}ms ({:+.2}ms change)",
                                    new_ms - old_ms
                                );
                            }
                            (None, Some(new)) => {
                                let new_ms = new.nseconds() as f64 / 1_000_000.0;
                                debug!("Latency established for Pipeline {pipeline_name:?}: {new_ms:.2}ms");
                                if new_ms > 100.0 {
                                    warn!("High latency detected for Pipeline {pipeline_name:?}: {:.2}ms - may cause noticeable delay", new_ms);
                                }
                            }
                            _ => {
                                debug!("Latency recalculation completed for Pipeline {pipeline_name:?}, no change in value");
                            }
                        }
                    }
                    Err(error) => {
                        warn!("Failed to recalculate latency for Pipeline {pipeline_name:?}: {error:?}");
                    }
                }
            }
            MessageView::Qos(qos) => {
                let (live, running_time, stream_time, timestamp, duration) = qos.get();
                let (jitter, proportion, quality) = qos.values();
                let (processed, dropped) = {
                    let stats = qos.stats();
                    (stats.0.value(), stats.1.value())
                };

                debug!(
                    concat!(
                        "QoS from {qos_src:?}:",
                        " live={live}",
                        " proportion={proportion:.2}",
                        " jitter={jitter:?}",
                        " quality={quality}",
                        " running_time={running_time:?}",
                        " stream_time={stream_time:?}",
                        " timestamp={timestamp:?}",
                        " duration={duration:?}",
                        " processed={processed}",
                        " dropped={dropped}",
                    ),
                    qos_src = qos.src().map(|s| s.path_string()),
                    live = live,
                    proportion = proportion,
                    jitter = jitter,
                    quality = quality,
                    running_time = running_time,
                    stream_time = stream_time,
                    timestamp = timestamp,
                    duration = duration,
                    processed = processed,
                    dropped = dropped
                );

                // Analyze QoS metrics for potential issues
                let mut issue_detected = false;
                let mut issue_reasons = Vec::new();

                // Check for low proportion (downstream is struggling)
                if proportion < 0.7 {
                    issue_reasons.push(format!("low proportion ({proportion:.2})",));
                    issue_detected = true;
                }

                // Check for high jitter (timing instability)
                if jitter < 0 && jitter.abs() > 50_000_000 {
                    // More than 50ms of negative jitter
                    issue_reasons.push(format!("high negative jitter ({jitter:?})",));
                    issue_detected = true;
                }

                // Check for frame drops
                if dropped > 0 {
                    issue_reasons.push(format!("{dropped} frames dropped"));
                    issue_detected = true;
                }

                // Check for quality degradation
                if quality < 0 {
                    issue_reasons.push(format!("quality degradation (quality={quality})"));
                    issue_detected = true;
                }

                // Log comprehensive QoS analysis
                if issue_detected {
                    warn!(
                        "QoS performance issue detected - Potential causes: {}",
                        issue_reasons.join(", ")
                    );
                }

                // Monitor trends over time
                if dropped > 0 {
                    let percentage = if processed > 0 {
                        (dropped as f64 / processed as f64) * 100.0
                    } else {
                        0.0
                    };
                    debug!(
                        "Frame drop rate: {dropped} frames dropped out of {processed} processed ({percentage:.1}%)",
                    );
                }
            }
            MessageView::NewClock(new_clock) => {
                if let Some(clock) = new_clock.clock() {
                    debug!(
                        "New clock selected: {:?} from {:?}",
                        clock.type_().name(),
                        new_clock.src().map(|s| s.path_string())
                    );
                } else {
                    debug!(
                        "New clock selected (clock not available) from {:?}",
                        new_clock.src().map(|s| s.path_string())
                    );
                }
            }
            MessageView::ClockLost(_) | MessageView::ClockProvide(_) => {
                warn!(
                    "Clock message received: {:?} - potential sync issues",
                    message.view()
                );
                // Force a new clock
                let _ = pipeline.set_state(gst::State::Paused);
                let _ = pipeline.set_state(gst::State::Playing);
            }
            // Ignored
            MessageView::Tag(_)
            | MessageView::AsyncDone(_)
            | MessageView::StreamStart(_)
            | MessageView::StreamStatus(_) => (),
            other_message => debug!("{other_message:#?}"),
        }
    }

    debug!("BusWatcher task ended for Pipeline {pipeline_name:?}!");
}

/// Extract the element topology from a running GStreamer pipeline as a directed graph.
///
/// Uses `iterate_recurse()` to include elements inside bins (e.g. rtspsrc,
/// webrtcbin internals). Each node records its parent bin and whether it is
/// itself a bin, enabling clients to reconstruct the structural hierarchy.
pub(crate) fn extract_topology(pipeline: &gst::Pipeline) -> PipelineTopology {
    let elements: Vec<gst::Element> = pipeline
        .iterate_recurse()
        .into_iter()
        .filter_map(Result::ok)
        .collect();

    let mut nodes = Vec::with_capacity(elements.len());
    let mut edges = Vec::new();

    for el in &elements {
        let name = el.name().to_string();
        let type_name = el
            .factory()
            .map(|f| f.name().to_string())
            .unwrap_or_default();

        let parent_bin: Option<String> = el
            .parent()
            .and_then(|p| p.downcast::<gst::Element>().ok())
            .filter(|p| !p.is::<gst::Pipeline>())
            .map(|p| p.name().to_string());

        let is_bin = el.downcast_ref::<gst::Bin>().is_some();

        nodes.push(TopologyNode {
            name: name.clone(),
            type_name,
            parent_bin,
            is_bin,
        });

        for pad in el.src_pads() {
            if let Some(peer) = pad.peer() {
                if let Some(peer_el) = peer.parent_element() {
                    let media_type = pad
                        .current_caps()
                        .and_then(|caps| caps.structure(0).map(|s| s.name().to_string()));

                    edges.push(TopologyEdge {
                        from_node: name.clone(),
                        from_pad: pad.name().to_string(),
                        to_node: peer_el.name().to_string(),
                        to_pad: peer.name().to_string(),
                        media_type,
                    });
                }
            }
        }
    }

    PipelineTopology {
        nodes,
        edges,
        sinks: Vec::new(),
    }
}
