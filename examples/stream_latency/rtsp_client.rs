use anyhow::{anyhow, Context, Result};
use gst::prelude::*;

use super::{attach_frame_probe, Codec, SampleSender};

pub fn create_rtsp_client(
    name: &str,
    url: &str,
    codec: Codec,
    sender: SampleSender,
) -> Result<gst::Pipeline> {
    let (depay, parse) = match codec {
        Codec::H264 => ("rtph264depay", "h264parse"),
        Codec::H265 => ("rtph265depay", "h265parse"),
    };

    let caps = match codec {
        Codec::H264 => "video/x-h264,stream-format=byte-stream,alignment=au",
        Codec::H265 => "video/x-h265,stream-format=byte-stream,alignment=au",
    };

    let description = format!(
        concat!(
            "rtspsrc location={url} is-live=true latency=0 do-retransmission=false udp-buffer-size=2621440",
            " ! {depay}",
            " ! {parse} name=parse config-interval=-1",
            " ! {caps}",
            " ! fakesink sync=false async=false",
        ),
        url = url,
        depay = depay,
        parse = parse,
        caps = caps,
    );

    let pipeline = gst::parse::launch(&description)
        .context("Failed to parse RTSP pipeline")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("Element is not a pipeline"))?;

    pipeline.set_property("name", name);

    let parse_elem = pipeline
        .by_name("parse")
        .ok_or_else(|| anyhow!("parse element not found"))?;

    let probe_pad = parse_elem.static_pad("src").unwrap();
    attach_frame_probe(&probe_pad, name.to_string(), sender);

    Ok(pipeline)
}
