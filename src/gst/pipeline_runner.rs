use gstreamer;
use gstreamer::prelude::*;

use crate::gst::gstreamer_runner;

use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct PipelineRunner {
    pipeline: String,
    gstreamer_pipeline: Arc<gstreamer::Element>,
    bus: Arc<gstreamer::Bus>,
    stop: Arc<Mutex<bool>>,
}

impl Default for PipelineRunner {
    fn default() -> Self {
        match gstreamer::init() {
            Ok(_) => {}
            Err(error) => {
                println!("Error! {}", error);
                std::process::exit(-1);
            }
        }

        let pipeline = "videotestsrc ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc ! rtph264pay ! udpsink host=0.0.0.0 port=5600".to_string();
        PipelineRunner {
            pipeline: pipeline.clone(),
            gstreamer_pipeline: Arc::new(gstreamer::parse_launch_full(&pipeline, None, gstreamer::ParseFlags::NONE).unwrap()),
            bus: Arc::new(gstreamer::Bus::new()),
            stop: Arc::new(Mutex::new(false))
        }
    }
}

impl PipelineRunner {
    pub fn set_pipeline(&mut self, pipeline: &str) {
        self.pipeline = String::from(pipeline);

        // Create pipeline from string
        let mut context = gstreamer::ParseContext::new();
        self.gstreamer_pipeline = match gstreamer::parse_launch_full(
            &self.pipeline,
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

        self.bus = Arc::new(self.gstreamer_pipeline.get_bus().unwrap());

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
        println!("change state: {:#?}", self.gstreamer_pipeline.set_state(gstreamer::State::Null));
        println!("stop : {:#?}", self.bus.post(&gstreamer::message::Message::new_eos().build()));
        println!("stop : {:#?}", self.gstreamer_pipeline.send_event(gstreamer::Event::new_eos().build()));
    }

    fn pipeline_loop(pipeline_runner: &PipelineRunner) {
        let pipeline = &pipeline_runner.gstreamer_pipeline;
        let bus = pipeline.get_bus().unwrap();

        println!("Bus {:#?} {:#?}", bus, pipeline_runner.bus);

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

            for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(10)) {
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
