use gstreamer;
use gstreamer::prelude::*;

use crate::gst::gstreamer_runner;

#[derive(Clone)]
pub struct PipelineRunner {
    pipeline: String,
}

impl Default for PipelineRunner {
    fn default() -> Self {
        PipelineRunner {
            pipeline: "videotestsrc ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc ! rtph264pay ! udpsink host=0.0.0.0 port=5600".to_string(),
        }
    }
}

impl PipelineRunner {
    pub fn set_pipeline(&mut self, pipeline: &str) {
        self.pipeline = String::from(pipeline);
    }

    pub fn run_loop(&self) {
        let self_structure = self.clone();
        let closure = move || PipelineRunner::pipeline_loop(self_structure);
        gstreamer_runner::run(closure);
    }

    fn pipeline_loop(pipeline_runner: PipelineRunner) {
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
            Ok(pipeline) => pipeline,
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

        pipeline
            .set_state(gstreamer::State::Playing)
            .expect("Unable to set the pipeline to the `Playing` state");

        for msg in bus.iter_timed(gstreamer::CLOCK_TIME_NONE) {
            use gstreamer::MessageView;

            match msg.view() {
                MessageView::Eos(..) => break,
                MessageView::Error(err) => {
                    println!(
                        "Error from {:?}: {} ({:?})",
                        err.get_src().map(|s| s.get_path_string()),
                        err.get_error(),
                        err.get_debug()
                    );
                    break;
                }
                _ => (),
            }
        }

        pipeline
            .set_state(gstreamer::State::Null)
            .expect("Unable to set the pipeline to the `Null` state");
    }
}
