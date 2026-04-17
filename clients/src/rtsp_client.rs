use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;

use crate::{attach_frame_probe, Codec, SampleSender, StreamClient};

pub struct RtspClient {
    pipeline: gst::Pipeline,
    frame_count: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl StreamClient for RtspClient {
    fn frames(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }
}

impl RtspClient {
    pub async fn new(url: &str, codec: Codec, sender: Option<SampleSender>) -> Result<Self> {
        gst::init()?;

        let parsed: url::Url = url.parse()?;
        let host = parsed.host_str().unwrap_or("127.0.0.1");
        let port = parsed.port().unwrap_or(8554);
        let addr = format!("{host}:{port}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            match tokio::net::TcpStream::connect(&addr).await {
                Ok(_) => break,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(e) => anyhow::bail!("RTSP port {addr} not reachable: {e}"),
            }
        }

        let description = match codec {
            Codec::H264 => format!(
                concat!(
                    "rtspsrc location={url} is-live=true latency=0 do-retransmission=false udp-buffer-size=2621440",
                    " ! rtph264depay",
                    " ! h264parse name=parse config-interval=-1",
                    " ! video/x-h264,stream-format=byte-stream,alignment=au",
                    " ! fakesink sync=false async=false",
                ),
                url = url,
            ),
            Codec::H265 => format!(
                concat!(
                    "rtspsrc location={url} is-live=true latency=0 do-retransmission=false udp-buffer-size=2621440",
                    " ! rtph265depay",
                    " ! h265parse name=parse config-interval=-1",
                    " ! video/x-h265,stream-format=byte-stream,alignment=au",
                    " ! fakesink sync=false async=false",
                ),
                url = url,
            ),
            Codec::Mjpg => format!(
                concat!(
                    "rtspsrc location={url} is-live=true latency=0 do-retransmission=false udp-buffer-size=2621440",
                    " ! rtpjpegdepay",
                    " ! identity name=parse",
                    " ! fakesink sync=false async=false",
                ),
                url = url,
            ),
            Codec::Yuyv | Codec::Rgb => format!(
                concat!(
                    "rtspsrc location={url} is-live=true latency=0 do-retransmission=false udp-buffer-size=2621440",
                    " ! rtpvrawdepay",
                    " ! identity name=parse",
                    " ! fakesink sync=false async=false",
                ),
                url = url,
            ),
        };

        let pipeline = gst::parse::launch(&description)
            .context("Failed to parse RTSP pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("Element is not a pipeline"))?;

        let parse_elem = pipeline
            .by_name("parse")
            .ok_or_else(|| anyhow!("parse element not found"))?;

        let frame_count = Arc::new(AtomicU64::new(0));
        let probe_pad = parse_elem.static_pad("src").unwrap();

        let counter = Arc::clone(&frame_count);
        probe_pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });

        if let Some(sender) = sender {
            let probe_pad = parse_elem.static_pad("src").unwrap();
            attach_frame_probe(&probe_pad, "rtsp-client".to_string(), sender);
        }

        pipeline.set_state(gst::State::Playing)?;

        Ok(Self {
            pipeline,
            frame_count,
        })
    }
}

impl Drop for RtspClient {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
