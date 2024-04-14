use gst::glib::clone;
use gst::prelude::*;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    gst::init()?;

    let opt_effects = std::env::var("EFFECTS").unwrap_or_else(|_| DEFAULT_EFFECTS.to_string());
    let effect_names: Vec<&str> = opt_effects.split(',').collect();

    let mut effects = Vec::new();
    for name in &effect_names {
        if let Some(element) = gst::ElementFactory::make(*name, None) {
            println!("Adding effect '{}'", name);
            effects.push(element);
        }
    }

    let pipeline = gst::Pipeline::new(None);
    let src = gst::ElementFactory::make("videotestsrc", None)?;
    src.set_property("is-live", &true)?;

    let filter = gst::ElementFactory::make("capsfilter", None)?;
    filter.set_property(
        "caps",
        &gst::Caps::builder("video/x-raw")
            .field("width", &320i32)
            .field("height", &240i32)
            .field(
                "format",
                &gst::List::new(&[
                    &"I420", &"YV12", &"YUY2", &"UYVY", &"AYUV", &"Y41B", &"Y42B", &"YVYU",
                    &"Y444", &"v210", &"v216", &"NV12", &"NV21", &"UYVP", &"A420", &"YUV9",
                    &"YVU9", &"IYU1",
                ]),
            )
            .build(),
    )?;

    let q1 = gst::ElementFactory::make("queue", None)?;
    let conv_before = gst::ElementFactory::make("videoconvert", None)?;

    let effect = effects.pop().expect("No effects specified");
    let cur_effect = effect.clone();

    let conv_after = gst::ElementFactory::make("videoconvert", None)?;
    let q2 = gst::ElementFactory::make("queue", None)?;
    let sink = gst::ElementFactory::make("ximagesink", None)?;

    pipeline.add_many(&[
        &src,
        &filter,
        &q1,
        &conv_before,
        &effect,
        &conv_after,
        &q2,
        &sink,
    ])?;
    gst::Element::link_many(&[
        &src,
        &filter,
        &q1,
        &conv_before,
        &effect,
        &conv_after,
        &q2,
        &sink,
    ])?;

    pipeline.set_state(gst::State::Playing)?;

    let main_loop = glib::MainLoop::new(None, false);
    let pipeline_weak = pipeline.downgrade();
    let blockpad = q1.get_static_pad("src").unwrap();
    blockpad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_, probe_info| {
        let pipeline = match pipeline_weak.upgrade() {
            Some(pipeline) => pipeline,
            None => return gst::PadProbeReturn::Ok,
        };

        let blockpad = pipeline
            .get_by_name("q1")
            .unwrap()
            .get_static_pad("src")
            .unwrap();
        blockpad.remove_probe(probe_info.id());

        let pipeline_weak = pipeline.downgrade();
        let cur_effect = cur_effect.clone();
        let effects = effects.clone();
        let blockpad_weak = blockpad.downgrade();
        let event_probe_cb = move |_, probe_info| {
            let pipeline = match pipeline_weak.upgrade() {
                Some(pipeline) => pipeline,
                None => return gst::PadProbeReturn::Ok,
            };

            if let Some(event) = probe_info.get_event() {
                if let Some(event_type) = event.type_() {
                    if event_type == gst::EventType::Eos {
                        let blockpad = match blockpad_weak.upgrade() {
                            Some(blockpad) => blockpad,
                            None => return gst::PadProbeReturn::Ok,
                        };
                        blockpad.remove_probe(probe_info.id());

                        let next_effect = match effects.last() {
                            Some(effect) => effect,
                            None => return gst::PadProbeReturn::Drop,
                        };
                        println!(
                            "Switching from '{}' to '{}'",
                            cur_effect.get_path_string(),
                            next_effect.get_path_string()
                        );
                        cur_effect.set_state(gst::State::Null)?;
                        pipeline.remove(&cur_effect)?;

                        let cur_effect = next_effect.clone();
                        pipeline.add(&cur_effect)?;
                        cur_effect.link(&conv_after)?;
                        pipeline.add(&cur_effect)?;

                        cur_effect.set_state(gst::State::Playing)?;

                        return gst::PadProbeReturn::Drop;
                    }
                }
            }
            gst::PadProbeReturn::Ok
        };

        let event_probe_cb = event_probe_cb.into_pad_probe();

        if let Some(src_pad) = cur_effect.get_static_pad("src") {
            src_pad.add_probe(
                gst::PadProbeType::BLOCK | gst::PadProbeType::EVENT_DOWNSTREAM,
                clone!(@weak main_loop, @weak pipeline => move |pad, info| {
                    event_probe_cb(pad, info)?;
                    Ok(gst::PadProbeReturn::Ok)
                }),
            )?;
        }

        let pipeline_weak = pipeline.downgrade();
        let blockpad = blockpad.downgrade();
        let pad_probe_cb = move |_, probe_info| {
            let pipeline = match pipeline_weak.upgrade() {
                Some(pipeline) => pipeline,
                None => return gst::PadProbeReturn::Ok,
            };
            let blockpad = match blockpad.upgrade() {
                Some(blockpad) => blockpad,
                None => return gst::PadProbeReturn::Ok,
            };
            blockpad.remove_probe(probe_info.id());

            let blockpad = blockpad.downgrade();
            blockpad.add_probe(
                gst::PadProbeType::BLOCK_DOWNSTREAM,
                clone!(@weak pipeline => move |_, _| {
                    let pipeline = match pipeline.upgrade() {
                        Some(pipeline) => pipeline,
                        None => return gst::PadProbeReturn::Ok,
                    };
                    let blockpad = match blockpad.upgrade() {
                        Some(blockpad) => blockpad,
                        None => return gst::PadProbeReturn::Ok,
                    };
                    blockpad.remove_probe(probe_info.id());

                    let cur_effect = match effects.last() {
                        Some(effect) => effect,
                        None => return gst::PadProbeReturn::Drop,
                    };

                    let sink_pad = cur_effect.get_static_pad("sink").unwrap();
                    sink_pad.send_event(gst::Event::new_eos()?)?;

                    gst::PadProbeReturn::Drop
                }),
            )?;
            Ok(gst::PadProbeReturn::Ok)
        };

        let pad_probe_cb = pad_probe_cb.into_pad_probe();
        blockpad.add_probe(
            gst::PadProbeType::BLOCK_DOWNSTREAM,
            clone!(@weak main_loop, @weak pipeline => move |_, probe_info| {
                pad_probe_cb(_, probe_info)?;
                Ok(gst::PadProbeReturn::Ok)
            }),
        )?;

        Ok(gst::PadProbeReturn::Ok)
    })?;

    let bus = pipeline.get_bus().unwrap();
    bus.add_watch(move |_, msg| {
        let pipeline = match pipeline_weak.upgrade() {
            Some(pipeline) => pipeline,
            None => return glib::Continue(false),
        };

        match msg.view() {
            gst::MessageView::Error(err) => {
                eprintln!(
                    "Error received from element {}: {}",
                    msg.get_src().get_path_string(),
                    err.get_error()
                );
                pipeline.set_state(gst::State::Null).unwrap();
                main_loop.quit();
            }
            _ => (),
        }

        glib::Continue(true)
    })?;

    let main_loop = glib::MainLoop::new(None, false);
    let main_loop_clone = main_loop.clone();
    glib::timeout_add_seconds_local(1, move || {
        let blockpad = blockpad.downgrade();
        blockpad
            .add_probe(
                gst::PadProbeType::BLOCK_DOWNSTREAM,
                clone!(@weak main_loop_clone => move |_, _| {
                    let main_loop = match main_loop_clone.upgrade() {
                        Some(main_loop) => main_loop,
                        None => return gst::PadProbeReturn::Ok,
                    };
                    main_loop.quit();
                    gst::PadProbeReturn::Drop
                }),
            )
            .unwrap();
        glib::Continue(true)
    });

    main_loop.run();

    Ok(())
}

const DEFAULT_EFFECTS: &str = "identity,exclusion,navigationtest,agingtv,videoflip,vertigotv,gaussianblur,shagadelictv,edgetv";
