use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use anyhow::{Context, Result};
use gst::prelude::*;
use tokio::task::JoinHandle;

use crate::{attach_frame_probe, Codec, SampleSender, StreamClient};

#[path = "../../src/lib/stream/sink/types/zenoh_message.rs"]
mod zenoh_message;
use zenoh_message::CompressedVideo;

pub struct ZenohClient {
    pipeline: gst::Pipeline,
    frame_count: Arc<AtomicU64>,
    _subscriber_handle: JoinHandle<()>,
}

#[async_trait::async_trait]
impl StreamClient for ZenohClient {
    fn frames(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }

    fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }
}

impl ZenohClient {
    pub async fn new(
        topic: &str,
        codec: Codec,
        sender: Option<SampleSender>,
        config: zenoh::Config,
    ) -> Result<Self> {
        gst::init()?;

        let pipeline = gst::Pipeline::with_name("zenoh-client");

        let (parse_factory, norm_caps) = match codec {
            Codec::H264 => (
                "h264parse",
                gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            ),
            Codec::H265 => (
                "h265parse",
                gst::Caps::builder("video/x-h265")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            ),
            other => anyhow::bail!("ZenohClient does not support {other:?}"),
        };

        let appsrc = gst_app::AppSrc::builder()
            .caps(&norm_caps)
            .is_live(true)
            .format(gst::Format::Time)
            .do_timestamp(false)
            .build();

        let parse = gst::ElementFactory::make(parse_factory)
            .build()
            .context(format!("Failed to create {parse_factory}"))?;

        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", &norm_caps)
            .build()
            .context("Failed to create capsfilter")?;

        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
            .context("Failed to create fakesink")?;

        let src_elem = appsrc.upcast_ref::<gst::Element>();
        pipeline.add_many([src_elem, &parse, &capsfilter, &sink])?;
        gst::Element::link_many([src_elem, &parse, &capsfilter, &sink])?;

        let frame_count = Arc::new(AtomicU64::new(0));
        let probe_pad = parse.static_pad("src").unwrap();

        let counter = Arc::clone(&frame_count);
        probe_pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });

        if let Some(sender) = sender {
            let probe_pad = parse.static_pad("src").unwrap();
            attach_frame_probe(&probe_pad, "zenoh-client".to_string(), sender);
        }

        let session = zenoh::open(config)
            .await
            .map_err(anyhow::Error::msg)
            .context("Failed to open zenoh session")?;

        let subscriber = session
            .declare_subscriber(topic)
            .await
            .map_err(anyhow::Error::msg)
            .context("Failed to declare zenoh subscriber")?;

        let handle = tokio::spawn(async move {
            let _session = session;

            while let Ok(sample) = subscriber.recv_async().await {
                let payload = sample.payload().to_bytes();
                let message: CompressedVideo = match cdr::deserialize::<CompressedVideo>(&payload) {
                    Ok(msg) => msg,
                    Err(error) => {
                        eprintln!("[zenoh-client] Failed to deserialize CDR message: {error}");
                        continue;
                    }
                };

                let pts = gst::ClockTime::from_nseconds(message.timestamp.to_nanos());
                let mut buffer = gst::Buffer::from_slice(message.data);
                buffer.get_mut().unwrap().set_pts(pts);

                if appsrc.push_buffer(buffer).is_err() {
                    eprintln!("[zenoh-client] Failed to push buffer into appsrc");
                    break;
                }
            }
        });

        pipeline.set_state(gst::State::Playing)?;

        Ok(Self {
            pipeline,
            frame_count,
            _subscriber_handle: handle,
        })
    }
}

impl Drop for ZenohClient {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self._subscriber_handle.abort();
    }
}
