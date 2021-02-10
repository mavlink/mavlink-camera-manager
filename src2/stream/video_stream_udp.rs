use super::gst::pipeline_runner::Pipeline;
use super::stream_backend::StreamBackend;

use std::sync::{Arc, Mutex};
use std::thread;

use gstreamer;
use gstreamer::prelude::*;

#[derive(Debug)]
struct VideoStreamUdpState {
    run: bool,
    kill: bool,
    pipeline: Pipeline,
}

pub struct VideoStreamUdp {
    state: Arc<Mutex<VideoStreamUdpState>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for VideoStreamUdpState {
    fn default() -> Self {
        Self {
            run: true,
            kill: false,
            pipeline: Default::default(),
        }
    }
}

impl Default for VideoStreamUdp {
    fn default() -> Self {
        let state: Arc<Mutex<VideoStreamUdpState>> = Default::default();
        let thread_state = state.clone();
        Self {
            state,
            thread: Some(thread::spawn(move || run_video_stream_udp(thread_state))),
        }
    }
}

impl Drop for VideoStreamUdp {
    fn drop(&mut self) {
        // Kill the thread and wait for it
        self.state.lock().unwrap().kill = true;
        self.thread.take().unwrap().join();
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
}

fn run_video_stream_udp(state: Arc<Mutex<VideoStreamUdpState>>) {
    if let Err(error) = gstreamer::init() {
        /*
        return Err(SimpleError::new(format!(
            "Failed to init GStreamer: {}",
            error
        )));
        */
    }

    let mut pipeline;

    'externalLoop: loop {
        println!("external");

        let pipeline_description = state.lock().unwrap().pipeline.description.clone();

        // Create pipeline from string
        let mut context = gstreamer::ParseContext::new();
        pipeline = match gstreamer::parse_launch_full(
            &pipeline_description,
            Some(&mut context),
            gstreamer::ParseFlags::empty(),
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                unreachable!();
                /*
                if let Some(gstreamer::ParseError::NoSuchElement) =
                    error.kind::<gstreamer::ParseError>()
                {
                    return Err(SimpleError::new(format!(
                        "GStreamer error: Missing element(s): {:?}",
                        context.get_missing_elements()
                    )));
                }
                return Err(SimpleError::new(format!(
                    "GStreamer error: Failed to parse pipeline: {}",
                    error
                )));
                */
            }
        };

        let bus = pipeline.get_bus().unwrap();

        if let Err(error) = pipeline.set_state(gstreamer::State::Playing) {
            /*
            return Err(SimpleError::new(format!(
                "GStreamer error: Unable to set the pipeline to the `Playing` state (check the bus for error messages): {}",
                error
            )));
            */
        }

        // Check if we need to break external loop
        'internalLoop: loop {
            println!("internal");
            if !state.lock().unwrap().run {
                break 'externalLoop;
            }

            for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                use gstreamer::MessageView;

                match msg.view() {
                    MessageView::Eos(eos) => {
                        /*
                        return Err(SimpleError::new(format!(
                            "GStreamer error: EOS received: {:#?}",
                            eos
                        )));
                        */
                    }
                    MessageView::Error(error) => {
                        /*
                        return Err(SimpleError::new(format!(
                            "GStreamer error: Error from {:?}: {} ({:?})",
                            error.get_src().map(|s| s.get_path_string()),
                            error.get_error(),
                            error.get_debug()
                        )));
                        */
                    }
                    _ => (),
                }
            }
        }

        let result = pipeline.set_state(gstreamer::State::Null);
        if result.is_err() {
            eprintln!("Unable to set the pipeline to the `Null` state");
        }

        // The loop will restart, add delay to avoid high cpu usage
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if let Err(error) = pipeline.set_state(gstreamer::State::Null) {
        /*
        return Err(SimpleError::new(format!(
            "GStreamer error: Unable to set the pipeline to the `Null` state: {}",
            error
        )));
        */
    }
}
