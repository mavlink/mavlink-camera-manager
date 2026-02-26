use anyhow::{Context, Result};
use gst::prelude::*;

use super::{attach_frame_probe, SampleSender};

pub fn create_rtsp_client(name: &str, url: &str, sender: SampleSender) -> Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::with_name(name);
    let client_name = name.to_string();

    let src = gst::ElementFactory::make("rtspsrc")
        .property("location", url)
        .property("latency", 0u32)
        .property("is-live", true)
        .property("do-retransmission", false)
        .build()
        .context("Failed to create rtspsrc")?;

    pipeline.add(&src)?;

    let pipeline_weak = pipeline.downgrade();
    src.connect_pad_added(move |_src, src_pad| {
        let caps = src_pad
            .current_caps()
            .or_else(|| Some(src_pad.query_caps(None)));
        let Some(caps) = caps else { return };
        let Some(structure) = caps.structure(0) else {
            return;
        };

        if structure.get::<&str>("media").unwrap_or("") != "video" {
            return;
        }

        let encoding = structure
            .get::<&str>("encoding-name")
            .unwrap_or("")
            .to_uppercase();

        let (depay_factory, parse_factory, norm_caps) = match encoding.as_str() {
            "H264" => (
                "rtph264depay",
                "h264parse",
                gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            ),
            "H265" => (
                "rtph265depay",
                "h265parse",
                gst::Caps::builder("video/x-h265")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            ),
            other => {
                eprintln!("[{client_name}] Unsupported encoding: {other}");
                return;
            }
        };

        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };

        let Ok(depay) = gst::ElementFactory::make(depay_factory).build() else {
            eprintln!("[{client_name}] Failed to create {depay_factory}");
            return;
        };
        let Ok(parse) = gst::ElementFactory::make(parse_factory).build() else {
            eprintln!("[{client_name}] Failed to create {parse_factory}");
            return;
        };
        let Ok(capsfilter) = gst::ElementFactory::make("capsfilter")
            .property("caps", &norm_caps)
            .build()
        else {
            return;
        };
        let Ok(sink) = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
        else {
            return;
        };

        pipeline.add_many([&depay, &parse, &capsfilter, &sink]).ok();
        gst::Element::link_many([&depay, &parse, &capsfilter, &sink]).ok();
        depay.sync_state_with_parent().ok();
        parse.sync_state_with_parent().ok();
        capsfilter.sync_state_with_parent().ok();
        sink.sync_state_with_parent().ok();

        let probe_pad = parse.static_pad("src").unwrap();
        attach_frame_probe(&probe_pad, client_name.clone(), sender.clone());

        let sink_pad = depay.static_pad("sink").unwrap();
        if src_pad.link(&sink_pad).is_err() {
            eprintln!("[{client_name}] Failed to link rtspsrc pad to depayloader");
        }
    });

    Ok(pipeline)
}
