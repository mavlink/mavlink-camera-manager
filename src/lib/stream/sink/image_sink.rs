use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Error, Result};
use gst::prelude::*;
use tracing::*;

use mcm_api::v1::{
    stream::{CaptureConfiguration, VideoAndStreamInformation},
    video::VideoEncodeType,
};

use crate::stream::pipeline::runner::PipelineRunner;

use super::{link_sink_to_tee, unlink_sink_from_tee, SinkInterface};

type ClonableResult<T> = Result<T, Arc<Error>>;

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct ThumbnailSettings {
    quality: u8,
    target_height: Option<u32>,
}

#[derive(Debug)]
struct Thumbnail {
    pub instant: std::time::Instant,
    pub image: Vec<u8>,
}

#[derive(Debug, Default)]
struct CachedThumbnails {
    map: HashMap<ThumbnailSettings, Thumbnail>,
}

impl CachedThumbnails {
    pub fn try_get(&self, settings: &ThumbnailSettings) -> Result<Option<Vec<u8>>> {
        if std::env::var("MCM_THUMBNAIL_NO_CACHE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            return Ok(None);
        }
        if let Some(thumbnail) = self.map.get(settings) {
            if std::time::Instant::now() - thumbnail.instant < std::time::Duration::from_secs(1) {
                return Ok(Some(thumbnail.image.to_vec()));
            }
        }

        Ok(None)
    }

    fn try_set(&mut self, settings: &ThumbnailSettings, image: Vec<u8>) -> Result<()> {
        self.map.insert(
            settings.to_owned(),
            Thumbnail {
                instant: std::time::Instant::now(),
                image,
            },
        );

        Ok(())
    }
}

#[derive(Debug)]
pub struct ImageSink {
    sink_id: Arc<uuid::Uuid>,
    pipeline: gst::Pipeline,
    queue: gst::Element,
    proxysink: gst::Element,
    _proxysrc: gst::Element,
    _pipeline_elements: Vec<gst::Element>,
    _appsink: gst_app::AppSink,
    jpegenc: gst::Element,
    scale_capsfilter: gst::Element,
    source_width: u32,
    source_height: u32,
    tee_src_pad: Arc<Mutex<Option<gst::Pad>>>,
    jpeg_sender: tokio::sync::broadcast::Sender<ClonableResult<Vec<u8>>>,
    pad_blocker: Arc<Mutex<Option<gst::PadProbeId>>>,
    pipeline_runner: PipelineRunner,
    thumbnails: Arc<Mutex<CachedThumbnails>>,
}

impl SinkInterface for ImageSink {
    #[instrument(level = "debug", skip(self, pipeline))]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        {
            let mut guard = self.tee_src_pad.lock().unwrap();
            if guard.is_some() {
                return Err(anyhow!(
                    "Tee's src pad from Sink {:?} has already been configured",
                    self.get_id()
                ));
            }
            guard.replace(tee_src_pad.clone());
        }

        let elements = &[&self.queue, &self.proxysink];
        link_sink_to_tee(&tee_src_pad, pipeline, elements)?;

        if let Some(queue_src_pad) = self.queue.static_pad("src") {
            if let Some(blocker_id) = queue_src_pad
                .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                    gst::PadProbeReturn::Ok
                })
            {
                *self.pad_blocker.lock().unwrap() = Some(blocker_id);
            }
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self, pipeline))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &Arc<uuid::Uuid>) -> Result<()> {
        let guard = self.tee_src_pad.lock().unwrap();
        let Some(tee_src_pad) = guard.as_ref() else {
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
            "Not available. Reason: Image Sink doesn't provide endpoints"
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

impl ImageSink {
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        sink_id: Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream")
            .property("silent", true)
            .property("flush-on-eos", true)
            .property("max-size-buffers", 1u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .build()?;

        let proxysink = gst::ElementFactory::make("proxysink").build()?;
        let _proxysrc = gst::ElementFactory::make("proxysrc")
            .property("proxysink", &proxysink)
            .build()?;

        // Configure proxysrc's internal queue for low-latency thumbnail capture
        match _proxysrc.downcast_ref::<gst::Bin>() {
            Some(bin) => {
                let elements = bin.children();
                match elements
                    .iter()
                    .find(|element| element.name().starts_with("queue"))
                {
                    Some(element) => {
                        element.set_property_from_str("leaky", "downstream");
                        element.set_property("silent", true);
                        element.set_property("flush-on-eos", true);
                        element.set_property("max-size-buffers", 1u32);
                        element.set_property("max-size-bytes", 0u32);
                        element.set_property("max-size-time", 0u64);
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

        let (source_width, source_height) = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(config) => (config.width, config.height),
            CaptureConfiguration::Redirect(_) => {
                return Err(anyhow!(
                    "PipelineRunner aborted: Redirect CaptureConfiguration means the stream was not initialized yet"
                ));
            }
        };

        let encoding = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(config) => config.encode.clone(),
            CaptureConfiguration::Redirect(_) => unreachable!(),
        };

        // Build encoding-specific decode elements
        let mut _pipeline_elements: Vec<gst::Element> = Default::default();
        match encoding {
            VideoEncodeType::H264 => {
                let filter = gst::ElementFactory::make("identity")
                    .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                    .property("sync", false)
                    .build()?;
                let decoder = gst::ElementFactory::make("avdec_h264")
                    .property_from_str("lowres", "2")
                    .build()?;
                decoder
                    .has_property("max-threads", None)
                    .then(|| decoder.set_property("max-threads", 1));
                decoder
                    .has_property("discard-corrupted-frames", None)
                    .then(|| decoder.set_property("discard-corrupted-frames", true));
                _pipeline_elements.push(filter);
                _pipeline_elements.push(decoder);
            }
            VideoEncodeType::H265 => {
                let filter = gst::ElementFactory::make("identity")
                    .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                    .property("sync", false)
                    .build()?;
                let decoder = gst::ElementFactory::make("avdec_h265")
                    .property_from_str("lowres", "2")
                    .build()?;
                decoder
                    .has_property("max-threads", None)
                    .then(|| decoder.set_property("max-threads", 1));
                decoder
                    .has_property("discard-corrupted-frames", None)
                    .then(|| decoder.set_property("discard-corrupted-frames", true));
                decoder
                    .has_property("std-compliance", None)
                    .then(|| decoder.set_property_from_str("std-compliance", "normal"));
                _pipeline_elements.push(filter);
                _pipeline_elements.push(decoder);
            }
            VideoEncodeType::Mjpg => {
                let decoder = gst::ElementFactory::make("jpegdec").build()?;
                decoder
                    .has_property("discard-corrupted-frames", None)
                    .then(|| decoder.set_property("discard-corrupted-frames", true));
                _pipeline_elements.push(decoder);
            }
            VideoEncodeType::Rgb | VideoEncodeType::Yuyv => {}
            _ => {
                return Err(anyhow!(
                    "Unsupported video encoding for ImageSink: {encoding:?}. The supported are: H264, H265, MJPG, RGB and YUYV"
                ))
            }
        };

        // videoscale for target resolution, single-threaded to minimize CPU
        let videoscale = gst::ElementFactory::make("videoscale")
            .property_from_str("method", "lanczos")
            .property("n-threads", 1u32)
            .build()?;
        _pipeline_elements.push(videoscale);

        // capsfilter to control the target resolution dynamically
        let scale_capsfilter = gst::ElementFactory::make("capsfilter").build()?;
        _pipeline_elements.push(scale_capsfilter.clone());

        // videoconvert to ensure format compatibility with jpegenc
        let videoconvert = gst::ElementFactory::make("videoconvert")
            .property("n-threads", 1u32)
            .build()?;
        _pipeline_elements.push(videoconvert);

        // jpegenc produces final JPEG bytes — quality is updated dynamically per request
        let jpegenc = gst::ElementFactory::make("jpegenc")
            .property("quality", 70i32)
            .property_from_str("idct-method", "ifast")
            .build()?;
        _pipeline_elements.push(jpegenc.clone());

        let pad_blocker: Arc<Mutex<Option<gst::PadProbeId>>> = Default::default();
        let pad_blocker_clone = pad_blocker.clone();
        let tee_src_pad: Arc<Mutex<Option<gst::Pad>>> = Default::default();
        let queue_clone = queue.clone();

        let (sender, _) = tokio::sync::broadcast::channel(1);
        let jpeg_sender = sender.clone();
        let mut pending = false;

        let appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                if sender.receiver_count() == 0 || pending {
                    return Ok(gst::FlowSuccess::Ok);
                }
                debug!("Starting a snapshot");
                pending = true;

                let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or_else(|| {
                    let reason = "Failed to get buffer from appsink";
                    gst::element_error!(appsink, gst::ResourceError::Failed, ("{reason:?}"));
                    let _ = sender.send(Err(Arc::new(anyhow!(reason))));
                    pending = false;
                    gst::FlowError::Error
                })?;

                // Re-block at the queue's src pad to stop data flow after capture
                if let Some(queue_src_pad) = queue_clone.static_pad("src") {
                    if let Some(old_blocker) = queue_src_pad
                        .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                            gst::PadProbeReturn::Ok
                        })
                        .and_then(|blocker| pad_blocker_clone.lock().unwrap().replace(blocker))
                    {
                        queue_src_pad.remove_probe(old_blocker);
                    }
                }

                let map = buffer.map_readable().map_err(|_| {
                    let reason = "Failed to map buffer readable";
                    gst::element_error!(appsink, gst::ResourceError::Failed, ("{reason:?}"));
                    let _ = sender.send(Err(Arc::new(anyhow!(reason))));
                    pending = false;
                    gst::FlowError::Error
                })?;

                let _ = sender.send(Ok(map.as_slice().to_vec()));
                pending = false;
                debug!("Finished the snapshot");

                Ok(gst::FlowSuccess::Ok)
            })
            .build();

        let appsink = gst_app::AppSink::builder()
            .name(format!("AppSink-{sink_id}"))
            .sync(false)
            .max_buffers(1u32)
            .drop(true)
            .callbacks(appsink_callbacks)
            .build();
        appsink.set_property("enable-last-sample", false);
        appsink.set_property("async", false);

        let pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-image-sink-{sink_id}"))
            .build();

        let mut elements = vec![&_proxysrc];
        elements.extend(_pipeline_elements.iter().collect::<Vec<&gst::Element>>());
        elements.push(appsink.upcast_ref());
        let elements = &elements;
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding ImageSink's elements to Sink Pipeline: {add_err:?}"
            ));
        }

        if let Err(link_err) = gst::Element::link_many(elements) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from ImageSink Pipeline: {remove_err:?}")
            };
            return Err(anyhow!("Failed linking ImageSink's elements: {link_err:?}"));
        }

        let pipeline_runner = PipelineRunner::try_new_background(
            &pipeline,
            &sink_id,
            true,
            video_and_stream_information,
        )?;

        Ok(Self {
            sink_id: sink_id.clone(),
            pipeline,
            queue,
            proxysink,
            _proxysrc,
            _pipeline_elements,
            _appsink: appsink,
            jpegenc,
            scale_capsfilter,
            source_width,
            source_height,
            tee_src_pad,
            jpeg_sender,
            pad_blocker,
            pipeline_runner,
            thumbnails: Default::default(),
        })
    }

    #[instrument(level = "debug", skip(self))]
    async fn try_get_jpeg(&self, quality: u8, target_height: Option<u32>) -> Result<Vec<u8>> {
        if self.pipeline.current_state() != gst::State::Playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
        }

        // Dynamically update jpegenc quality
        self.jpegenc.set_property("quality", quality as i32);

        // Dynamically update the target resolution capsfilter
        if let Some(target_h) = target_height {
            let aspect_ratio = self.source_width as f64 / self.source_height as f64;
            let target_w = (target_h as f64 * aspect_ratio) as u32;
            // Ensure even dimensions for YUV format compatibility
            let target_w = target_w + (target_w % 2);
            let target_h = target_h + (target_h % 2);

            let caps = gst::Caps::builder("video/x-raw")
                .field("width", target_w as i32)
                .field("height", target_h as i32)
                .build();
            self.scale_capsfilter.set_property("caps", &caps);
        } else {
            self.scale_capsfilter
                .set_property("caps", &gst::Caps::new_any());
        }

        // Unblock the queue's src pad to allow data to flow to proxysink
        if let Some(blocker) = self.pad_blocker.lock().unwrap().take() {
            if let Some(queue_src_pad) = self.queue.static_pad("src") {
                queue_src_pad.remove_probe(blocker);
            }
        }

        // Request an immediate keyframe from the upstream encoder so the
        // decoder can produce a frame without waiting for the next natural
        // keyframe (which may be many seconds away with x264enc defaults).
        // The event must be sent on a src pad: GStreamer's send_event()
        // only accepts upstream events on src pads.
        if let Some(queue_src_pad) = self.queue.static_pad("src") {
            let event = gst_video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            queue_src_pad.send_event(event);
        }

        let mut receiver = self.jpeg_sender.subscribe();

        tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver.recv())
            .await??
            .map_err(|e| anyhow!(e.to_string()))
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn make_jpeg_thumbnail_from_last_frame(
        &self,
        quality: u8,
        target_height: Option<u32>,
    ) -> Result<Vec<u8>> {
        let settings = ThumbnailSettings {
            quality,
            target_height,
        };

        // Try to get from cache
        {
            let thumbnails = match self.thumbnails.lock() {
                Ok(guard) => guard,
                Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
            };

            if let Some(thumbnail) = thumbnails.try_get(&settings)? {
                return Ok(thumbnail);
            };
        }

        let jpeg_bytes = self.try_get_jpeg(quality, target_height).await?;

        // Cache the result
        {
            let mut thumbnails = match self.thumbnails.lock() {
                Ok(guard) => guard,
                Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
            };

            if let Err(error) = thumbnails.try_set(&settings, jpeg_bytes.clone()) {
                error!("Failed setting cached thumbnail. Reason: {error:?}");
            }
        }

        Ok(jpeg_bytes)
    }
}
