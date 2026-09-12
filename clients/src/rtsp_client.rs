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
    pub async fn new(
        url: &str,
        codec: Codec,
        sender: Option<SampleSender>,
        connect_timeout: Duration,
    ) -> Result<Self> {
        anyhow::ensure!(!url.is_empty(), "RTSP url must be non-empty");
        gst::init()?;

        let parsed: url::Url = url.parse()?;
        let host = parsed.host_str().unwrap_or("127.0.0.1");
        let port = parsed.port().unwrap_or(8554);
        let addr = format!("{host}:{port}");
        let deadline = tokio::time::Instant::now() + connect_timeout;
        let mut last_error = None;
        while let Err(error) = tokio::net::TcpStream::connect(&addr).await {
            last_error = Some(error);
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("RTSP port {addr} not reachable: {}", last_error.unwrap());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let rtspsrc = format!(
            "rtspsrc location={url} is-live=true latency=0 do-retransmission=false udp-buffer-size=2621440"
        );
        let tail = match codec {
            Codec::H264 => {
                "rtph264depay ! h264parse name=parse config-interval=-1 \
                 ! video/x-h264,stream-format=byte-stream,alignment=au \
                 ! fakesink sync=false async=false"
            }
            Codec::H265 => {
                "rtph265depay ! h265parse name=parse config-interval=-1 \
                 ! video/x-h265,stream-format=byte-stream,alignment=au \
                 ! fakesink sync=false async=false"
            }
            Codec::Mjpg => "rtpjpegdepay ! identity name=parse ! fakesink sync=false async=false",
            Codec::Yuyv | Codec::Rgb => {
                "rtpvrawdepay ! identity name=parse ! fakesink sync=false async=false"
            }
        };
        let description = format!("{rtspsrc} ! {tail}");

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
            attach_frame_probe(&probe_pad, "rtsp-client".to_string(), sender, codec);
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
