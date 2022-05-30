use std::sync::{Arc, Mutex};
use std::thread;

use gstreamer::prelude::*;
use gstreamer::{self, MessageView};

use log::debug;

use crate::stream::stream_backend::StreamBackend;

use super::pipeline_builder::Pipeline;

#[derive(Debug, Default)]
pub struct PipelineRunnerState {
    // move run kill restart logic to enum as states
    pipeline: Pipeline,
    run: bool,
    kill: bool,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct PipelineRunner {
    pub state: Arc<Mutex<PipelineRunnerState>>,
    thread: Option<std::thread::JoinHandle<()>>,
    thread_rx_channel: Option<std::sync::mpsc::Receiver<String>>,
}

#[allow(dead_code)]
impl PipelineRunner {
    pub fn new(pipeline: Pipeline) -> Self {
        Self {
            state: Arc::new(Mutex::new(PipelineRunnerState {
                pipeline,
                ..Default::default()
            })),
            thread: None,
            thread_rx_channel: None,
        }
    }

    pub fn run(&mut self) -> std::sync::mpsc::Receiver<String> {
        let state = self.state.clone();
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        self.thread = Some(thread::spawn(move || {
            pipeline_runner(state, sender);
        }));
        return receiver;
    }
}

impl StreamBackend for PipelineRunner {
    fn pipeline(&self) -> String {
        let string = self.state.lock().unwrap().pipeline.description.clone();
        return string;
    }

    fn start(&mut self) -> bool {
        self.run();
        self.state.lock().unwrap().run = true;
        return true;
    }

    fn stop(&mut self) -> bool {
        self.state.lock().unwrap().run = false;
        return true;
    }

    fn restart(&mut self) {
        unimplemented!();
    }

    fn is_running(&self) -> bool {
        return self.state.lock().unwrap().run;
    }

    fn allow_same_endpoints(&self) -> bool {
        false
    }
}

impl Drop for PipelineRunner {
    fn drop(&mut self) {
        // Kill the thread and wait for it
        self.state.lock().unwrap().kill = true;

        if let Some(thread) = self.thread.take() {
            let answer = thread.join();
            debug!("done: {:#?}", answer);
        };
    }
}

fn pipeline_runner(
    state: Arc<Mutex<PipelineRunnerState>>,
    channel_tx: std::sync::mpsc::Sender<String>,
) {
    if let Err(error) = gstreamer::init() {
        let _ = channel_tx.send(format!("Failed to init GStreamer: {}", error));
        return;
    }

    let mut pipeline: Option<gstreamer::Element> = None;
    //TODO: move to while not kill
    'externalLoop: loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if state.lock().unwrap().kill {
            break 'externalLoop;
        }
        if !state.lock().unwrap().run {
            continue;
        }

        let pipeline_description = state.lock().unwrap().pipeline.description.clone();

        // Create pipeline from string
        let mut context = gstreamer::ParseContext::new();
        pipeline = match gstreamer::parse_launch_full(
            &pipeline_description,
            Some(&mut context),
            gstreamer::ParseFlags::empty(),
        ) {
            Ok(pipeline) => Some(pipeline),
            Err(error) => {
                if let Some(gstreamer::ParseError::NoSuchElement) =
                    error.kind::<gstreamer::ParseError>()
                {
                    let _ = channel_tx.send(format!(
                        "GStreamer error: Missing element(s): {:?}",
                        context.missing_elements()
                    ));
                } else {
                    let _ = channel_tx.send(format!(
                        "GStreamer error: Failed to parse pipeline: {}",
                        error
                    ));
                }
                continue;
            }
        };

        let bus = pipeline.as_ref().unwrap().bus().unwrap();

        if let Err(error) = pipeline
            .as_ref()
            .unwrap()
            .set_state(gstreamer::State::Playing)
        {
            let _ = channel_tx.send(format!(
                "GStreamer error: Unable to set the pipeline to the `Playing` state (check the bus for error messages): {}",
                error
            ));
            continue;
        }

        // Create dot file for the pipeline
        gstreamer::debug_bin_to_dot_file(
            pipeline
                .as_ref()
                .unwrap()
                .downcast_ref::<gstreamer::Pipeline>()
                .unwrap(),
            gstreamer::DebugGraphDetails::all(),
            "pipeline-started-video-stream-udp",
        );

        // Check if we need to break external loop.
        // Some cameras have a duplicated timestamp when starting.
        // to avoid restarting the camera once and once again,
        // this checks for a maximum of 10 lost before restarting.
        let mut previous_position: Option<gstreamer::ClockTime> = None;
        let mut lost_timestamps: usize = 0;
        let max_lost_timestamps: usize = 10;

        'innerLoop: loop {
            if state.lock().unwrap().kill {
                break 'externalLoop;
            }
            if !state.lock().unwrap().run {
                break 'innerLoop;
            }

            // Restart pipeline if pipeline position do not change,
            // occur if usb connection is lost and gstreamer do not detect it
            match pipeline
                .as_ref()
                .unwrap()
                .query_position::<gstreamer::ClockTime>()
            {
                Some(position) => {
                    previous_position = match previous_position {
                        Some(current_previous_position) => {
                            if current_previous_position.nseconds() != 0
                                && current_previous_position.nseconds() == position.nseconds()
                            {
                                lost_timestamps += 1;
                                let message =
                                    format!("Position did not change {}", lost_timestamps);
                                let _ = channel_tx.send(message);
                                let _ = channel_tx
                                    .send("Lost camera communication, restarting pipeline!".into());
                            } else {
                                // We are back in track, erase lost timestamps
                                lost_timestamps = 0;
                            }

                            if lost_timestamps > max_lost_timestamps {
                                break 'innerLoop;
                            }

                            Some(position)
                        }
                        None => Some(position),
                    }
                }
                None => {}
            }

            for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                match msg.view() {
                    MessageView::Eos(eos) => {
                        let message = format!("GStreamer error: EOS received: {:#?}", eos);
                        let _ = channel_tx.send(message);
                        break 'innerLoop;
                    }
                    MessageView::Error(error) => {
                        let message = format!(
                            "GStreamer error: Error from {:?}: {} ({:?})",
                            error.src().map(|s| s.path_string()),
                            error.error(),
                            error.debug()
                        );
                        let _ = channel_tx.send(message);
                        break 'innerLoop;
                    }
                    _ => (),
                }
            }
        }

        if let Err(error) = pipeline.as_ref().unwrap().set_state(gstreamer::State::Null) {
            let _ = channel_tx.send(format!(
                "GStreamer error: Unable to set the pipeline to the `Null` state: {:#?}",
                error
            ));
        }

        // The loop will restart, add delay to avoid high cpu usage
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if pipeline.as_ref().is_some() {
        if let Err(error) = pipeline.as_ref().unwrap().set_state(gstreamer::State::Null) {
            let _ = channel_tx.send(format!(
                "GStreamer error: Unable to set the pipeline to the `Null` state: {:#?}",
                error
            ));
        }
    }
}
