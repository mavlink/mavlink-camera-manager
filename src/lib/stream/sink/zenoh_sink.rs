use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tokio::task::JoinHandle;
use tracing::*;

use crate::{
    stream::{pipeline::runner::PipelineRunner, types::CaptureConfiguration},
    video::types::VideoEncodeType,
    video_stream::types::VideoAndStreamInformation,
    zenoh::foxglove_messages::{CompressedVideo, Timestamp},
};

use super::{link_sink_to_tee, unlink_sink_from_tee, SinkInterface};

#[derive(Debug)]
pub struct ZenohSink {
    sink_id: Arc<uuid::Uuid>,
    pipeline: gst::Pipeline,
    queue: gst::Element,
    proxysink: gst::Element,
    _proxysrc: gst::Element,
    _parser: gst::Element,
    _appsink: gst_app::AppSink,
    tee_src_pad: Option<gst::Pad>,
    pipeline_runner: PipelineRunner,
    _topic: String,
    _publisher_task_handle: JoinHandle<()>,
}

impl Drop for ZenohSink {
    fn drop(&mut self) {
        self._publisher_task_handle.abort();
    }
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
    #[instrument(level = "debug", skip_all)]
    pub async fn try_new(
        sink_id: Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
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

        let video_encoding = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(video_configuraiton) => video_configuraiton.encode.clone(),
            CaptureConfiguration::Redirect(_) => {
                return Err(anyhow!(
                    "Redirect CaptureConfiguration means the stream was not initialized yet"
                ));
            }
        };

        let _parser;
        let caps;
        let encode_type;
        match video_encoding {
            VideoEncodeType::H264 => {
                encode_type = "h264";
                _parser = gst::ElementFactory::make("h264parse").build()?;
                _parser.set_property("config-interval", -1i32);
                caps = gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build();
            }
            VideoEncodeType::H265 => {
                encode_type = "h265";
                _parser = gst::ElementFactory::make("h265parse").build()?;
                _parser.set_property("config-interval", -1i32);
                caps = gst::Caps::builder("video/x-h265")
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build();
            }
            _ => {
                return Err(anyhow!(
                    "Unsupported video encoding for ZenohSink: {video_encoding:?}. The supported are: H264 and H265"
                ))
            }
        }

        let video_source_name = video_and_stream_information
            .name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();
        let _topic = format!("video/{video_source_name}/stream");

        let (sender, _publisher_task_handle) = {
            let session = crate::zenoh::get().context("No Zenoh Session found")?;
            let zenoh_topic = session
                .declare_keyexpr(_topic.clone())
                .await
                .map_err(anyhow::Error::msg)?;
            let zenoh_encoding =
                zenoh::bytes::Encoding::APPLICATION_CDR.with_schema("foxglove.CompressedVideo");
            let publisher = session
                .declare_publisher(zenoh_topic)
                .encoding(zenoh_encoding)
                .await
                .map_err(anyhow::Error::msg)?;

            let (sender, mut receiver) = tokio::sync::mpsc::channel::<CompressedVideo>(100);
            let publisher_task_handle = tokio::spawn(async move {
                let topic = publisher.key_expr().to_string();

                while let Some(message) = receiver.recv().await {
                    let encoded_data =
                        match cdr::serialize::<_, _, cdr::CdrLe>(&message, cdr::Infinite) {
                            Ok(encoded) => encoded,
                            Err(error) => {
                                error!("Failed to serialize message: {error:?}");
                                continue;
                            }
                        };

                    if let Err(error) = publisher.put(encoded_data).await {
                        error!("Failed sending frame to zenoh topic {topic:?}: {error:?}");
                    }
                }

                if let Err(error) = publisher.undeclare().await {
                    error!("Failed undeclaring zenoh publisher {topic:?}: {error:?}");
                }
            });

            (sender, publisher_task_handle)
        };

        // Create the appsink callbacks
        let appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample({
                let frame_id = "vehicle".to_string();
                let format = encode_type.to_string();
                let sender = sender.clone();

                move |appsink| {
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

                    let buffer_map = match buffer.map_readable() {
                        Ok(map) => map,
                        Err(error) => {
                            error!("Error mapping buffer: {error:?}");
                            return Ok(gst::FlowSuccess::Ok);
                        }
                    };
                    let buffer_map_len = buffer_map.len();

                    // Only send if we have valid data (1MB limit)
                    if !buffer_map.is_empty() && buffer_map_len < 1024 * 1024 {
                        trace!("Publishing {format:?} frame with size: {buffer_map_len:?}");

                        // https://docs.foxglove.dev/docs/visualization/message-schemas/compressed-video
                        let message = CompressedVideo {
                            timestamp: Timestamp::now(),
                            frame_id: frame_id.clone(),
                            data: buffer_map.to_vec(),
                            format: format.clone(),
                        };

                        if let Err(error) = sender.blocking_send(message) {
                            error!("Error sending data through channel: {error}");
                        }
                    } else {
                        warn!("Skipping invalid frame: size={buffer_map_len:?}");
                    }

                    Ok(gst::FlowSuccess::Ok)
                }
            })
            .build();

        let _appsink = gst_app::AppSink::builder()
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

        let elements = &vec![&_proxysrc, &_parser, &_appsink.upcast_ref()];
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding ZenohSink's elements to Pipeline: {add_err:?}"
            ));
        }

        if let Err(link_err) = gst::Element::link_many(elements) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from ZenohSink Pipeline: {remove_err:?}")
            };
            return Err(anyhow!("Failed linking ZenohSink's elements: {link_err:?}"));
        }

        let pipeline_runner =
            PipelineRunner::try_new(&pipeline, &sink_id, false, video_and_stream_information)?;

        Ok(Self {
            sink_id,
            pipeline,
            queue,
            proxysink,
            _proxysrc,
            _parser,
            _appsink,
            tee_src_pad: Default::default(),
            pipeline_runner,
            _topic,
            _publisher_task_handle,
        })
    }
}
