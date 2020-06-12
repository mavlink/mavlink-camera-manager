use gstreamer;
use gstreamer::prelude::*;

use crate::gst::gstreamer_runner;

use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct PipelineRunner {
    pipeline: String,
    stop: Arc<Mutex<bool>>,
}

impl Default for PipelineRunner {
    fn default() -> Self {
        PipelineRunner {
            pipeline: "videotestsrc ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc ! rtph264pay ! udpsink host=0.0.0.0 port=5600".to_string(),
            stop: Arc::new(Mutex::new(false))
        }
    }
}

impl PipelineRunner {
    pub fn set_pipeline(&mut self, pipeline: &str) {
        self.pipeline = String::from(pipeline);
        *self.stop.lock().unwrap() = false;
    }

    pub fn run_loop(&self) {
        //let self_structure = self.clone();
        //let closure = move || PipelineRunner::pipeline_loop(self);
        //gstreamer_runner::run(closure);
        PipelineRunner::pipeline_loop(self);
    }

    pub fn stop(&self) {
        *self.stop.lock().unwrap() = true;
    }

    fn pipeline_loop(pipeline_runner: &PipelineRunner) {
        match gstreamer::init() {
            Ok(_) => {}
            Err(error) => {
                println!("Error! {}", error);
                std::process::exit(-1);
            }
        }

        // Create pipeline from string
        let mut context = gstreamer::ParseContext::new();
        let pipeline = match gstreamer::parse_launch_full(
            &pipeline_runner.pipeline,
            Some(&mut context),
            gstreamer::ParseFlags::NONE,
        ) {
            Ok(pipeline) => Arc::new(pipeline),
            Err(err) => {
                if let Some(gstreamer::ParseError::NoSuchElement) =
                    err.kind::<gstreamer::ParseError>()
                {
                    println!(
                        "Error: Missing element(s): {:?}",
                        context.get_missing_elements()
                    );
                } else {
                    println!("Error: Failed to parse pipeline: {}", err);
                }
                std::process::exit(-1)
            }
        };

        let bus = pipeline.get_bus().unwrap();

        let result = pipeline.set_state(gstreamer::State::Playing);
        if result.is_err() {
            eprintln!(
                "Unable to set the pipeline to the `Playing` state (check the bus for error messages)."
            );
        }

        'externalLoop: loop {
            if *pipeline_runner.stop.lock().unwrap() == true {
                break;
            }

            for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                use gstreamer::MessageView;

                match msg.view() {
                    MessageView::Eos(..) => {
                        println!("EOS!");
                        break 'externalLoop;
                    },
                    MessageView::Error(err) => {
                        print!(
                            "Error from {:?}: {} ({:?})\n",
                            err.get_src().map(|s| s.get_path_string()),
                            err.get_error(),
                            err.get_debug()
                        );
                        break;
                    }
                    _ => (),
                }
            }
        }

        let result = pipeline.set_state(gstreamer::State::Null);
        if result.is_err() {
            eprintln!("Unable to set the pipeline to the `Null` state");
        }
    }
}
