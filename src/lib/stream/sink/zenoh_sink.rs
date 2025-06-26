use lazy_static::lazy_static;
use std::sync::Arc;
use tokio::sync::Mutex;

use anyhow::{anyhow, Result};
use gst::prelude::*;
use tokio::sync::mpsc;
use tracing::*;

use crate::{
    cli::manager::zenoh_config_file, foxglove, stream::pipeline::runner::PipelineRunner,
    video::types::VideoEncodeType,
};

use super::{link_sink_to_tee, unlink_sink_from_tee, SinkInterface};

lazy_static! {
    static ref ZENOH_SESSION: Mutex<Option<zenoh::Session>> = Mutex::new(None);
}

#[derive(Debug)]
pub struct ZenohSink {
    sink_id: Arc<uuid::Uuid>,
    pipeline: gst::Pipeline,
    queue: gst::Element,
    proxysink: gst::Element,
    _proxysrc: gst::Element,
    _parser: gst::Element,
    appsink: gst_app::AppSink,
    tee_src_pad: Option<gst::Pad>,
    pipeline_runner: PipelineRunner,
    topic_suffix: String,
}

impl SinkInterface for ZenohSink {
    #[instrument(level = "debug", skip(self, pipeline))]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from Sink {:?} has already been configured",
                self.get_id()
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        let elements = &[&self.queue, &self.proxysink];
        link_sink_to_tee(tee_src_pad, pipeline, elements)?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self, pipeline))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &Arc<uuid::Uuid>) -> Result<()> {
        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink Sink from a pipeline without a Tee src pad.");
            return Ok(());
        };

        let elements = &[&self.queue, &self.proxysink];
        unlink_sink_from_tee(tee_src_pad, pipeline, elements)?;

        if let Err(error) = self.pipeline.set_state(::gst::State::Null) {
            warn!("Failed setting sink Pipeline state to Null: {error:?}");
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn get_id(&self) -> Arc<uuid::Uuid> {
        self.sink_id.clone()
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available. Reason: Zenoh Sink doesn't provide endpoints"
        ))
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        self.pipeline_runner.start()
    }

    #[instrument(level = "debug", skip(self))]
    fn eos(&self) {
        let pipeline_weak = self.pipeline.downgrade();
        if let Err(error) = std::thread::Builder::new()
            .name("EOS".to_string())
            .spawn(move || {
                let pipeline = pipeline_weak.upgrade().unwrap();
                if let Err(error) = pipeline.post_message(gst::message::Eos::new()) {
                    error!("Failed posting Eos message into Sink bus. Reason: {error:?}");
                }
            })
            .expect("Failed spawning EOS thread")
            .join()
        {
            error!(
                "EOS Thread Panicked with: {:?}",
                error.downcast_ref::<String>()
            );
        }
    }

    fn pipeline(&self) -> Option<&gst::Pipeline> {
        Some(&self.pipeline)
    }
}

impl ZenohSink {
    #[instrument(level = "debug")]
    pub async fn try_new(
        sink_id: Arc<uuid::Uuid>,
        encoding: VideoEncodeType,
        topic_suffix: String,
    ) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("silent", true)
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .name(format!("queue-zenoh-{sink_id}"))
            .build()?;

        // Create a pair of proxies. The proxysink will be used in the source's pipeline,
        // while the proxysrc will be used in this sink's pipeline
        let proxysink = gst::ElementFactory::make("proxysink").build()?;
        let _proxysrc = gst::ElementFactory::make("proxysrc")
            .property("proxysink", &proxysink)
            .build()?;

        // Configure proxysrc's queue, skips if fails
        match _proxysrc.downcast_ref::<gst::Bin>() {
            Some(bin) => {
                let elements = bin.children();
                match elements
                    .iter()
                    .find(|element| element.name().starts_with("queue"))
                {
                    Some(element) => {
                        element.set_property_from_str("leaky", "downstream"); // Throw away any data
                        element.set_property("silent", true);
                        element.set_property("flush-on-eos", true);
                        element.set_property("max-size-buffers", 0u32); // Disable buffers
                    }
                    None => {
                        warn!("Failed to customize proxysrc's queue: Failed to find queue in proxysrc");
                    }
                }
            }
            None => {
                warn!("Failed to customize proxysrc's queue: Failed to downcast element to bin")
            }
        }

        let _parser;
        let caps;
        let encode_type;
        match encoding {
            VideoEncodeType::H264 => {
                encode_type = "h264";
                _parser = gst::ElementFactory::make("h264parse").build()?;
                caps = gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build();
            }
            VideoEncodeType::H265 => {
                encode_type = "h265";
                _parser = gst::ElementFactory::make("h265parse").build()?;
                caps = gst::Caps::builder("video/x-h265")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build();
            }
            _ => {
                return Err(anyhow!(
                "Unsupported video encoding for ImageSink: {encoding:?}. The supported are: H264 and H265"
            ))
            }
        }

        let mut zenoh_session_guard = ZENOH_SESSION.lock().await;
        if zenoh_session_guard.is_none() {
            let config = if let Some(zenoh_config_file) = zenoh_config_file() {
                zenoh::Config::from_file(zenoh_config_file)
                    .map_err(|error| anyhow!("Failed to load Zenoh config file: {error:?}"))?
            } else {
                let mut config = zenoh::Config::default();
                config
                    .insert_json5("mode", r#""client""#)
                    .expect("Failed to insert client mode");
                config
                    .insert_json5("connect/endpoints", r#"["tcp/127.0.0.1:7447"]"#)
                    .expect("Failed to insert endpoints");
                config
            };

            let session = zenoh::open(config)
                .await
                .map_err(|error| anyhow!("Failed to open Zenoh session: {error:?}"))?;
            *zenoh_session_guard = Some(session);
        }
        let zenoh_session = zenoh_session_guard.as_ref().unwrap();

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(100);

        // Spawn a task to handle the video data publishing
        tokio::spawn({
            let topic_suffix = topic_suffix.clone();
            let zenoh_session = zenoh_session.clone();
            async move {
                while let Some(data) = rx.recv().await {
                    let compressed_video = foxglove::CompressedVideo {
                        timestamp: foxglove::Time::default(),
                        frame_id: "vehicle".to_string(),
                        data: data.clone(),
                        format: encode_type.to_string(),
                    };

                    let json = serde_json::to_string(&compressed_video).unwrap();
                    let json_len = json.len();

                    let cbor = compressed_video.to_cbor().unwrap();
                    let cbor_len = cbor.len();

                    let flatbuffer = compressed_video.to_flatbuffer().unwrap();
                    let flatbuffers_len = flatbuffer.len();

                    let ros2 = compressed_video.to_ros2().unwrap();
                    let ros2_len = ros2.len();

                    let cdr = compressed_video.to_cdr().unwrap();
                    let cdr_len = cdr.len();

                    let protobuf = compressed_video.to_protobuf().unwrap();
                    let protobuf_len = protobuf.len();

                    let data_len = data.len();

                    println!(
                        "LENGTHS: data: {} | JSON: {} ({:.2}x) | CBOR: {} ({:.2}x) | FlatBuffers: {} ({:.2}x) | ROS2: {} ({:.2}x) | CDR: {} ({:.2}x) | PROTOBUF: {} ({:.2}x)",
                        data_len,
                        json_len, json_len as f64 / data_len as f64,
                        cbor_len, cbor_len as f64 / data_len as f64,
                        flatbuffers_len, flatbuffers_len as f64 / data_len as f64,
                        ros2_len, ros2_len as f64 / data_len as f64,
                        cdr_len, cdr_len as f64 / data_len as f64,
                        protobuf_len, protobuf_len as f64 / data_len as f64,
                    );

                    // println!("ros2: {:?}", &ros2[0..100]);
                    // println!("cdr: {:?}", &cdr[0..100]);
                    // println!("protobuf: {:?}", &protobuf[0..100]);

                    if let Err(error) = zenoh_session
                        .put(&format!("video/{topic_suffix}/stream"), protobuf)
                        .encoding(zenoh::bytes::Encoding::APPLICATION_PROTOBUF)
                        .await
                    {
                        error!("Error publishing data: {error:?}");
                    }
                }
            }
        });

        // Create the appsink callbacks
        let tx_clone = tx.clone();
        let appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                let sample = match appsink.pull_sample() {
                    Ok(sample) => sample,
                    Err(error) => {
                        error!("Error pulling sample: {error:?}");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                };

                let buffer = match sample.buffer() {
                    Some(buffer) => buffer,
                    None => {
                        error!("No buffer in sample");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                };

                let map = match buffer.map_readable() {
                    Ok(map) => map,
                    Err(error) => {
                        error!("Error mapping buffer: {error:?}");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                };

                let data = map.as_slice().to_vec();
                trace!("Publishing H.264 frame with size: {}", data.len());

                // Only send if we have valid data
                if !data.is_empty() && data.len() < 1024 * 1024 {
                    // 1MB limit
                    if let Err(error) = tx_clone.blocking_send(data) {
                        error!("Error sending data through channel: {error}");
                    }
                } else {
                    warn!("Skipping invalid frame: size={}", data.len());
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build();

        let appsink = gst_app::AppSink::builder()
            .name(format!("AppSink-{sink_id}"))
            .sync(false)
            .max_buffers(1u32)
            .drop(true)
            .caps(&caps)
            .callbacks(appsink_callbacks)
            .build();

        let pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-zenoh-sink-{sink_id}"))
            .build();

        let elements = &vec![&_proxysrc, &_parser, &appsink.upcast_ref()];
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding ZenohSink's elements to Pipeline: {add_err:?}"
            ));
        }

        if let Err(link_err) = gst::Element::link_many(elements) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from ImageSink Pipeline: {remove_err:?}")
            };
            return Err(anyhow!("Failed linking ImageSink's elements: {link_err:?}"));
        }

        let pipeline_runner = PipelineRunner::try_new(&pipeline, &sink_id, false)?;

        if let Err(state_err) = pipeline.set_state(gst::State::Playing) {
            return Err(anyhow!(
                "Failed starting ZenohSink's pipeline: {state_err:#?}"
            ));
        }

        Ok(Self {
            sink_id,
            pipeline,
            queue,
            proxysink,
            _proxysrc,
            _parser,
            appsink,
            tee_src_pad: Default::default(),
            pipeline_runner,
            topic_suffix,
        })
    }
}
