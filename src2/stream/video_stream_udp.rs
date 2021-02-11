use super::gst::pipeline_runner::Pipeline;
use super::stream_backend::StreamBackend;

use std::sync::{Arc, Mutex};
use std::thread;

use gstreamer;
use gstreamer::prelude::*;

#[derive(Debug)]
struct VideoStreamUdpState {
    // move run kill restart logic to enum as states
    run: bool,
    kill: bool,
    pipeline: Pipeline,
}

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
            pipeline: Default::default(),
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
        println!("done: {:#?}", answer);
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

    fn set_pipeline_description(&mut self, description: &'static str) {
        self.state.lock().unwrap().pipeline.description = description.into();
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
    'externalLoop: loop {
        std::thread::sleep(std::time::Duration::from_millis(1000));
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
                        context.get_missing_elements()
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

        let bus = pipeline.as_ref().unwrap().get_bus().unwrap();

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

        // Check if we need to break external loop
        'innerLoop: loop {
            if state.lock().unwrap().kill {
                break 'externalLoop;
            }
            if !state.lock().unwrap().run {
                break 'innerLoop;
            }

            for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                use gstreamer::MessageView;

                match msg.view() {
                    MessageView::Eos(eos) => {
                        let _ = channel.send(format!("GStreamer error: EOS received: {:#?}", eos));
                        break 'innerLoop;
                    }
                    MessageView::Error(error) => {
                        let _ = channel.send(format!(
                            "GStreamer error: Error from {:?}: {} ({:?})",
                            error.get_src().map(|s| s.get_path_string()),
                            error.get_error(),
                            error.get_debug()
                        ));
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
