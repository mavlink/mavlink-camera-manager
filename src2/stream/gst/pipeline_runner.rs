use gstreamer;
use gstreamer::prelude::*;

use std::sync::{mpsc, Arc, Mutex};
use std::thread;

#[derive(Clone)]
pub struct Pipeline {
    pub description: String,
}

#[derive(Default)]
pub struct PipelineRunner {
    pub pipeline: Pipeline,
    pub stop: bool,
}

impl Default for Pipeline {
    fn default() -> Self {
        Pipeline {
            description: "videotestsrc pattern=ball ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5600".into(),
        }
    }
}

impl PipelineRunner {
    pub fn run(&self) {
        pipeline_loop(self);
    }
}

struct PipelineThreadRunner {
    private_channel: mpsc::Receiver<Pipeline>,
    public_channel: mpsc::Sender<Pipeline>,
}

fn pipeline_loop(pipeline_runner: &PipelineRunner) {
    match gstreamer::init() {
        Ok(_) => {}
        Err(error) => {
            println!("Error! {}", error);
            std::process::exit(-1);
        }
    }

    let mut pipeline;

    'externalLoop: loop {
        println!("external");

        // Create pipeline from string
        let mut context = gstreamer::ParseContext::new();
        pipeline = match gstreamer::parse_launch_full(
            &pipeline_runner.pipeline.description,
            Some(&mut context),
            gstreamer::ParseFlags::empty(),
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

        // Check if we need to break external loop
        'internalLoop: loop {
            println!("internal");
            if pipeline_runner.stop == true {
                break 'externalLoop;
            }

            for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
                use gstreamer::MessageView;

                match msg.view() {
                    MessageView::Eos(..) => {
                        println!("EOS received.");
                        // Broke internal loop to restart pipeline
                        break 'internalLoop;
                    }
                    MessageView::Error(err) => {
                        print!(
                            "Error from {:?}: {} ({:?})\n",
                            err.get_src().map(|s| s.get_path_string()),
                            err.get_error(),
                            err.get_debug()
                        );
                        break 'internalLoop;
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

    let result = pipeline.set_state(gstreamer::State::Null);
    if result.is_err() {
        eprintln!("Unable to set the pipeline to the `Null` state");
    }
}
