use gstreamer;
use gstreamer::prelude::*;

use simple_error::SimpleError;

#[derive(Clone, Debug)]
pub struct Pipeline {
    pub description: String,
}

pub struct PipelineRunner {
    pub pipeline: Pipeline,
}

#[allow(dead_code)]
impl PipelineRunner {
    pub fn run(&self) -> Result<(), SimpleError> {
        simple_pipeline_loop(self)
    }
}

fn simple_pipeline_loop(pipeline_runner: &PipelineRunner) -> Result<(), SimpleError> {
    if let Err(error) = gstreamer::init() {
        return Err(SimpleError::new(format!(
            "Failed to init GStreamer: {}",
            error
        )));
    }

    let mut context = gstreamer::ParseContext::new();

    let pipeline: Result<gstreamer::Element, SimpleError> = match gstreamer::parse_launch_full(
        &pipeline_runner.pipeline.description,
        Some(&mut context),
        gstreamer::ParseFlags::empty(),
    ) {
        Ok(pipeline) => Ok(pipeline),
        Err(error) => {
            if let Some(gstreamer::ParseError::NoSuchElement) =
                error.kind::<gstreamer::ParseError>()
            {
                return Err(SimpleError::new(format!(
                    "GStreamer error: Missing element(s): {:?}",
                    context.missing_elements()
                )));
            }
            return Err(SimpleError::new(format!(
                "GStreamer error: Failed to parse pipeline: {}",
                error
            )));
        }
    };
    let pipeline = pipeline?;

    let bus = pipeline.bus().unwrap();

    if let Err(error) = pipeline.set_state(gstreamer::State::Playing) {
        return Err(SimpleError::new(format!(
            "GStreamer error: Unable to set the pipeline to the `Playing` state (check the bus for error messages): {}",
            error
        )));
    }

    loop {
        for msg in bus.timed_pop(gstreamer::ClockTime::from_mseconds(100)) {
            use gstreamer::MessageView;

            match msg.view() {
                MessageView::Eos(eos) => {
                    return Err(SimpleError::new(format!(
                        "GStreamer error: EOS received: {:#?}",
                        eos
                    )));
                }
                MessageView::Error(error) => {
                    return Err(SimpleError::new(format!(
                        "GStreamer error: Error from {:?}: {} ({:?})",
                        error.src().map(|s| s.path_string()),
                        error.error(),
                        error.debug()
                    )));
                }
                _ => (),
            }
        }
    }
}

/*
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

    if let Err(error) = pipeline.set_state(gstreamer::State::Null) {
        return Err(SimpleError::new(format!(
            "GStreamer error: Unable to set the pipeline to the `Null` state: {}",
            error
        )));
    }
}*/
