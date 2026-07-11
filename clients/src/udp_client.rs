use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use anyhow::{Context, Result};
use gst::prelude::*;

use crate::{attach_frame_probe, Codec, SampleSender, StreamClient};

pub struct UdpClient {
    pipeline: gst::Pipeline,
    frame_count: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl StreamClient for UdpClient {
    fn frames(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }
}

impl UdpClient {
    pub fn new(
        address: &str,
        port: i32,
        codec: Codec,
        sender: Option<SampleSender>,
    ) -> Result<Self> {
        Self::build(address, port, codec, sender, None)
    }

    pub fn new_raw(
        address: &str,
        port: i32,
        codec: Codec,
        width: u32,
        height: u32,
        sender: Option<SampleSender>,
    ) -> Result<Self> {
        Self::build(address, port, codec, sender, Some((width, height)))
    }

    fn build(
        address: &str,
        port: i32,
        codec: Codec,
        sender: Option<SampleSender>,
        dimensions: Option<(u32, u32)>,
    ) -> Result<Self> {
        gst::init()?;

        let pipeline = gst::Pipeline::with_name("udp-client");

        let rtp_caps = match codec {
            Codec::H264 => gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("clock-rate", 90000i32)
                .field("encoding-name", "H264")
                .build(),
            Codec::H265 => gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("clock-rate", 90000i32)
                .field("encoding-name", "H265")
                .build(),
            Codec::Mjpg => gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("clock-rate", 90000i32)
                .field("encoding-name", "JPEG")
                .build(),
            Codec::Yuyv => {
                let (w, h) = dimensions.context("YUYV UDP requires dimensions via new_raw()")?;
                gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("clock-rate", 90000i32)
                    .field("encoding-name", "RAW")
                    .field("sampling", "YCbCr-4:2:0")
                    .field("depth", "8")
                    .field("width", w.to_string())
                    .field("height", h.to_string())
                    .build()
            }
            Codec::Rgb => {
                let (w, h) = dimensions.context("RGB UDP requires dimensions via new_raw()")?;
                gst::Caps::builder("application/x-rtp")
                    .field("media", "video")
                    .field("clock-rate", 90000i32)
                    .field("encoding-name", "RAW")
                    .field("sampling", "RGB")
                    .field("depth", "8")
                    .field("width", w.to_string())
                    .field("height", h.to_string())
                    .build()
            }
        };

        let src = gst::ElementFactory::make("udpsrc")
            .property("address", address)
            .property("port", port)
            .property("caps", &rtp_caps)
            .build()
            .context("Failed to create udpsrc")?;

        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
            .context("Failed to create fakesink")?;

        let probe_element = match codec {
            Codec::H264 | Codec::H265 => {
                let depay_factory = match codec {
                    Codec::H264 => "rtph264depay",
                    _ => "rtph265depay",
                };
                let parse_factory = match codec {
                    Codec::H264 => "h264parse",
                    _ => "h265parse",
                };
                let norm_caps = match codec {
                    Codec::H264 => gst::Caps::builder("video/x-h264")
                        .field("stream-format", "byte-stream")
                        .field("alignment", "au")
                        .build(),
                    _ => gst::Caps::builder("video/x-h265")
                        .field("stream-format", "byte-stream")
                        .field("alignment", "au")
                        .build(),
                };

                let depay = gst::ElementFactory::make(depay_factory)
                    .build()
                    .context(format!("Failed to create {depay_factory}"))?;
                let parse = gst::ElementFactory::make(parse_factory)
                    .build()
                    .context(format!("Failed to create {parse_factory}"))?;
                let capsfilter = gst::ElementFactory::make("capsfilter")
                    .property("caps", &norm_caps)
                    .build()
                    .context("Failed to create capsfilter")?;

                pipeline.add_many([&src, &depay, &parse, &capsfilter, &sink])?;
                gst::Element::link_many([&src, &depay, &parse, &capsfilter, &sink])?;
                parse
            }
            Codec::Mjpg => {
                let depay = gst::ElementFactory::make("rtpjpegdepay")
                    .build()
                    .context("Failed to create rtpjpegdepay")?;

                pipeline.add_many([&src, &depay, &sink])?;
                gst::Element::link_many([&src, &depay, &sink])?;
                depay
            }
            Codec::Yuyv | Codec::Rgb => {
                let depay = gst::ElementFactory::make("rtpvrawdepay")
                    .build()
                    .context("Failed to create rtpvrawdepay")?;

                pipeline.add_many([&src, &depay, &sink])?;
                gst::Element::link_many([&src, &depay, &sink])?;
                depay
            }
        };

        let frame_count = Arc::new(AtomicU64::new(0));
        let probe_pad = probe_element.static_pad("src").unwrap();

        let counter = Arc::clone(&frame_count);
        probe_pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });

        if let Some(sender) = sender {
            let probe_pad = probe_element.static_pad("src").unwrap();
            attach_frame_probe(&probe_pad, "udp-client".to_string(), sender);
        }

        pipeline.set_state(gst::State::Playing)?;

        Ok(Self {
            pipeline,
            frame_count,
        })
    }
}

impl Drop for UdpClient {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
