use super::gst::pipeline_runner::Pipeline;
use super::stream_backend::StreamBackend;

use std::sync::{Arc, Mutex};
use std::thread;

use gstreamer;
use gstreamer::prelude::*;
use gstreamer::MessageView;
use log::*;

#[derive(Debug)]
struct VideoStreamUdpState {
    // move run kill restart logic to enum as states
    run: bool,
    kill: bool,
    pipeline: Pipeline,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct VideoStreamUdp {
    state: Arc<Mutex<VideoStreamUdpState>>,
    thread: Option<std::thread::JoinHandle<()>>,
    thread_rx_channel: std::sync::mpsc::Receiver<String>,
}

impl Default for VideoStreamUdpState {
    fn default() -> Self {
        Self {
            run: false,
            kill: false,
            pipeline: Pipeline {
                description: "videotestsrc pattern=blink ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5600".into(),
            },
        }
    }
}

impl Default for VideoStreamUdp {
    fn default() -> Self {
        let state: Arc<Mutex<VideoStreamUdpState>> = Default::default();
        let thread_state = state.clone();
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        Self {
            state,
            thread: Some(thread::spawn(move || {
                run_video_stream_udp(thread_state.clone(), sender)
            })),
            thread_rx_channel: receiver,
        }
    }
}

impl Drop for VideoStreamUdp {
    fn drop(&mut self) {
        // Kill the thread and wait for it
        self.state.lock().unwrap().kill = true;
        let answer = self.thread.take().unwrap().join();
        debug!("done: {:#?}", answer);
    }
}

impl StreamBackend for VideoStreamUdp {
    fn start(&mut self) -> bool {
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

    fn set_pipeline_description(&mut self, description: &str) {
        self.state.lock().unwrap().pipeline.description = description.to_string();
    }

    fn pipeline(&self) -> String {
        let string = self.state.lock().unwrap().pipeline.description.clone();
        return string;
    }
}

fn run_video_stream_udp(
    state: Arc<Mutex<VideoStreamUdpState>>,
    channel: std::sync::mpsc::Sender<String>,
) {
    if let Err(error) = gstreamer::init() {
        let _ = channel.send(format!("Failed to init GStreamer: {}", error));
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
                    let _ = channel.send(format!(
                        "GStreamer error: Missing element(s): {:?}",
                        context.missing_elements()
                    ));
                } else {
                    let _ = channel.send(format!(
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
            let _ = channel.send(format!(
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
                                let _ = channel.send(message);
                                let _ = channel
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
                        let _ = channel.send(message);
                        break 'innerLoop;
                    }
                    MessageView::Error(error) => {
                        let message = format!(
                            "GStreamer error: Error from {:?}: {} ({:?})",
                            error.src().map(|s| s.path_string()),
                            error.error(),
                            error.debug()
                        );
                        let _ = channel.send(message);
                        break 'innerLoop;
                    }
                    _ => (),
                }
            }
        }

        if let Err(error) = pipeline.as_ref().unwrap().set_state(gstreamer::State::Null) {
            let _ = channel.send(format!(
                "GStreamer error: Unable to set the pipeline to the `Null` state: {:#?}",
                error
            ));
        }

        // The loop will restart, add delay to avoid high cpu usage
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if pipeline.as_ref().is_some() {
        if let Err(error) = pipeline.as_ref().unwrap().set_state(gstreamer::State::Null) {
            let _ = channel.send(format!(
                "GStreamer error: Unable to set the pipeline to the `Null` state: {:#?}",
                error
            ));
        }
    }
}
