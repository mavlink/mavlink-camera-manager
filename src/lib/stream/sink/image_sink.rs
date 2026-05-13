use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Error, Result};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::{
        gst::utils::try_set_property, pipeline::runner::PipelineRunner, types::CaptureConfiguration,
    },
    video::types::VideoEncodeType,
    video_stream::types::VideoAndStreamInformation,
};

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
        if let Some(thumbnail) = self.map.get(settings)
            && std::time::Instant::now() - thumbnail.instant < std::time::Duration::from_secs(1) {
                return Ok(Some(thumbnail.image.to_vec()));
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

    fn clear(&mut self) {
        self.map.clear();
    }
}

#[derive(Debug)]
pub struct ImageSink {
    sink_id: Arc<uuid::Uuid>,
    // Capture pipeline: proxysrc -> [decoders] -> appsink(enable-last-sample)
    pipeline: gst::Pipeline,
    queue: gst::Element,
    proxysink: gst::Element,
    _proxysrc: gst::Element,
    _transcoding_elements: Vec<gst::Element>,
    appsink: gst_app::AppSink,
    source_width: u32,
    source_height: u32,
    tee_src_pad: Arc<Mutex<Option<gst::Pad>>>,
    pad_blocker: Arc<Mutex<Option<gst::PadProbeId>>>,
    pipeline_runner: PipelineRunner,
    flat_samples_sender: tokio::sync::broadcast::Sender<ClonableResult<()>>,
    // Encoding pipeline: appsrc -> videoscale -> capsfilter -> videoconvert -> jpegenc -> jpeg_appsink
    encode_pipeline: gst::Pipeline,
    encode_appsrc: gst_app::AppSrc,
    _encode_elements: Vec<gst::Element>,
    jpegenc: gst::Element,
    scale_capsfilter: gst::Element,
    encode_sender: tokio::sync::broadcast::Sender<ClonableResult<Vec<u8>>>,
    // Level 1 cache (raw frame): GStreamer's last-sample on appsink + freshness tracking
    raw_capture_mutex: Arc<tokio::sync::Mutex<()>>,
    last_capture_instant: Arc<Mutex<Option<std::time::Instant>>>,
    // Level 2 cache (JPEG per settings)
    encode_mutex: Arc<tokio::sync::Mutex<()>>,
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

        if let Err(error) = self.encode_pipeline.set_state(::gst::State::Null) {
            warn!("Failed setting encode pipeline state to Null: {error:?}");
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
        let encode_pipeline_weak = self.encode_pipeline.downgrade();
        if let Err(error) = std::thread::Builder::new()
            .name("EOS".to_string())
            .spawn(move || {
                if let Some(pipeline) = pipeline_weak.upgrade()
                    && let Err(error) = pipeline.post_message(gst::message::Eos::new()) {
                        error!("Failed posting Eos message into Sink bus. Reason: {error:?}");
                    }
                if let Some(pipeline) = encode_pipeline_weak.upgrade()
                    && let Err(error) = pipeline.post_message(gst::message::Eos::new()) {
                        error!(
                            "Failed posting Eos message into encode pipeline bus. Reason: {error:?}"
                        );
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

        let (encoding, source_width, source_height) = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(video_configuraiton) => (
                video_configuraiton.encode.clone(),
                video_configuraiton.width,
                video_configuraiton.height,
            ),
            CaptureConfiguration::Redirect(_) => {
                return Err(anyhow!(
                    "PipelineRunner aborted: Redirect CaptureConfiguration means the stream was not initialized yet"
                ));
            }
        };

        // Depending of the sources' format we need different elements to transform it into a raw format
        let mut _transcoding_elements: Vec<gst::Element> = Default::default();
        match encoding {
            VideoEncodeType::H264 => {
                // For h264, we need to filter-out unwanted non-key frames here, before decoding it.
                let filter = gst::ElementFactory::make("identity")
                    .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                    .property("sync", false)
                    .build()?;
                let decoder = gst::ElementFactory::make("avdec_h264").build()?;
                try_set_property(&decoder, "lowres", 2); // (0) is 'full'; (1) is '1/2-size'; (2) is '1/4-size'
                try_set_property(&decoder, "skip-frame", 32); // (0) is 'default'; (8) is 'non-ref'; (16) is 'bidir'; (24) is 'non-intra'; (32) is 'non-key'; (48) is 'all'
                try_set_property(&decoder, "max-threads", 1);
                try_set_property(&decoder, "min-force-key-unit-interval", 100_000_000u64);
                try_set_property(&decoder, "qos", false);
                try_set_property(&decoder, "discard-corrupted-frames", true);
                try_set_property(&decoder, "output-corrupt", false);
                try_set_property(&decoder, "std-compliance", 0); // (2147483647) is 'auto'; (2) is 'very-strict'; (1) is 'strict'; (0) is 'normal'; (-1) is 'unofficial'; (-2) is 'experimental'
                _transcoding_elements.push(filter);
                _transcoding_elements.push(decoder);
            }
            VideoEncodeType::H265 => {
                // For h265, we need to filter-out unwanted non-key frames here, before decoding it.
                let filter = gst::ElementFactory::make("identity")
                .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                .property("sync", false)
                .build()?;
                let decoder = gst::ElementFactory::make("avdec_h265").build()?;
                try_set_property(&decoder, "lowres", 2); // (0) is 'full'; (1) is '1/2-size'; (2) is '1/4-size'
                try_set_property(&decoder, "skip-frame", 32); // (0) is 'default'; (8) is 'non-ref'; (16) is 'bidir'; (24) is 'non-intra'; (32) is 'non-key'; (48) is 'all'
                try_set_property(&decoder, "max-threads", 1);
                try_set_property(&decoder, "min-force-key-unit-interval", 100_000_000u64);
                try_set_property(&decoder, "qos", false);
                try_set_property(&decoder, "discard-corrupted-frames", true);
                try_set_property(&decoder, "output-corrupt", false);
                try_set_property(&decoder, "std-compliance", 0); // (2147483647) is 'auto'; (2) is 'very-strict'; (1) is 'strict'; (0) is 'normal'; (-1) is 'unofficial'; (-2) is 'experimental'
                _transcoding_elements.push(filter);
                _transcoding_elements.push(decoder);
            }
            VideoEncodeType::Mjpg => {
                let decoder = gst::ElementFactory::make("jpegdec").build()?;
                try_set_property(&decoder, "idct-method", 2); // (0) is 'islow'; (1) is 'ifast'; (2) is 'float'
                try_set_property(&decoder, "min-force-key-unit-interval", 100_000_000u64);
                try_set_property(&decoder, "qos", false);
                try_set_property(&decoder, "discard-corrupted-frames", true);
                _transcoding_elements.push(decoder);
            }
            VideoEncodeType::Rgb | VideoEncodeType::Yuyv => {}
            _ => {
                return Err(anyhow!(
                    "Unsupported video encoding for ImageSink: {encoding:?}. The supported are: H264, H265, MJPG, RGB and YUYV"
                ))
            }
        };

        // Raw capture appsink + callback
        let pad_blocker: Arc<Mutex<Option<gst::PadProbeId>>> = Default::default();
        let pad_blocker_clone = pad_blocker.clone();
        let tee_src_pad: Arc<Mutex<Option<gst::Pad>>> = Default::default();
        let queue_clone = queue.clone();

        let (sender, _) = tokio::sync::broadcast::channel(1);
        let flat_samples_sender = sender.clone();
        let mut pending = false;

        let appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |_appsink| {
                // Always re-block to stop data flow regardless of listener state
                if let Some(queue_src_pad) = queue_clone.static_pad("src") {
                    // Got a valid frame, block any further frame until next request
                    if let Some(old_blocker) = queue_src_pad
                        .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                            gst::PadProbeReturn::Ok
                        })
                        .and_then(|blocker| pad_blocker_clone.lock().unwrap().replace(blocker))
                    {
                        queue_src_pad.remove_probe(old_blocker);
                    }
                }

                // Only process if requested
                if sender.receiver_count() == 0 || pending {
                    return Ok(gst::FlowSuccess::Ok);
                }
                pending = true;

                // Send the data
                let _ = sender.send(Ok(()));
                pending = false;
                debug!("Raw frame captured into last-sample");

                Ok(gst::FlowSuccess::Ok)
            })
            .build();

        let appsink = gst_app::AppSink::builder()
            .name(format!("AppSink-{sink_id}"))
            .async_(false)
            .sync(false)
            .max_buffers(1u32)
            .drop(true)
            .enable_last_sample(true)
            .qos(false)
            .callbacks(appsink_callbacks)
            .build();
        try_set_property(appsink.upcast_ref(), "leaky-type", 2); // (0) is 'none'; (1) is 'upstream'; (2) is 'downstream'
        try_set_property(appsink.upcast_ref(), "silent", true);

        // Create the pipeline
        let pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-image-sink-{sink_id}"))
            .build();

        // Add Sink elements to the Sink's Pipeline
        let mut elements = vec![&_proxysrc];
        elements.extend(_transcoding_elements.iter().collect::<Vec<&gst::Element>>());
        elements.push(appsink.upcast_ref());
        let elements = &elements;
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding ImageSink's elements to Sink Pipeline: {add_err:?}"
            ));
        }

        // Link Sink's elements
        if let Err(link_err) = gst::Element::link_many(elements) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from ImageSink Pipeline: {remove_err:?}")
            };
            return Err(anyhow!("Failed linking ImageSink's elements: {link_err:?}"));
        }

        let pipeline_runner =
            PipelineRunner::try_new(&pipeline, &sink_id, true, video_and_stream_information)?;

        // Build encoding pipeline
        let (enc_sender, _) = tokio::sync::broadcast::channel(1);
        let encode_sender = enc_sender.clone();

        let encode_appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or_else(|| {
                    let _ = enc_sender.send(Err(Arc::new(anyhow!(
                        "Failed to get buffer from encode appsink"
                    ))));
                    gst::FlowError::Error
                })?;
                let map = buffer.map_readable().map_err(|_| {
                    let _ = enc_sender.send(Err(Arc::new(anyhow!(
                        "Failed to map encode buffer readable"
                    ))));
                    gst::FlowError::Error
                })?;
                let _ = enc_sender.send(Ok(map.as_slice().to_vec()));
                Ok(gst::FlowSuccess::Ok)
            })
            .build();

        let encode_appsrc = gst_app::AppSrc::builder()
            .name(format!("EncodeAppSrc-{sink_id}"))
            .format(gst::Format::Time)
            .is_live(false)
            .build();
        encode_appsrc.set_property("block", false);

        let enc_videoscale = gst::ElementFactory::make("videoscale")
            .property_from_str("method", "1")
            .property("n-threads", 1u32)
            .build()?;

        let scale_capsfilter = gst::ElementFactory::make("capsfilter").build()?;

        let enc_videoconvert = gst::ElementFactory::make("videoconvert")
            .property("n-threads", 1u32)
            .build()?;

        let jpegenc = gst::ElementFactory::make("jpegenc")
            .property("quality", 70i32)
            .property_from_str("idct-method", "1")
            .build()?;

        let encode_appsink = gst_app::AppSink::builder()
            .name(format!("EncodeAppSink-{sink_id}"))
            .async_(false)
            .sync(false)
            .max_buffers(1u32)
            .drop(false)
            .enable_last_sample(false)
            .qos(false)
            .callbacks(encode_appsink_callbacks)
            .build();
        try_set_property(encode_appsink.upcast_ref(), "silent", true);

        let _encode_elements: Vec<gst::Element> = vec![
            enc_videoscale,
            scale_capsfilter.clone(),
            enc_videoconvert,
            jpegenc.clone(),
        ];

        let encode_pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-image-encode-{sink_id}"))
            .build();

        let mut enc_elements: Vec<&gst::Element> = vec![encode_appsrc.upcast_ref()];
        enc_elements.extend(_encode_elements.iter());
        enc_elements.push(encode_appsink.upcast_ref());
        let enc_elements = &enc_elements;

        if let Err(add_err) = encode_pipeline.add_many(enc_elements) {
            return Err(anyhow!(
                "Failed adding elements to encode pipeline: {add_err:?}"
            ));
        }
        if let Err(link_err) = gst::Element::link_many(enc_elements) {
            if let Err(remove_err) = encode_pipeline.remove_many(enc_elements) {
                warn!("Failed removing elements from encode pipeline: {remove_err:?}")
            };
            return Err(anyhow!(
                "Failed linking encode pipeline elements: {link_err:?}"
            ));
        }

        Ok(Self {
            sink_id: sink_id.clone(),
            pipeline,
            queue,
            proxysink,
            _proxysrc,
            _transcoding_elements,
            appsink,
            source_width,
            source_height,
            tee_src_pad,
            pad_blocker,
            pipeline_runner,
            flat_samples_sender,
            encode_pipeline,
            encode_appsrc,
            _encode_elements,
            jpegenc,
            scale_capsfilter,
            encode_sender,
            raw_capture_mutex: Default::default(),
            last_capture_instant: Default::default(),
            encode_mutex: Default::default(),
            thumbnails: Default::default(),
        })
    }

    /// Unblock the source pad, wait for a single raw frame to arrive at
    /// `appsink` (stored via `enable-last-sample`), then re-block.
    #[instrument(level = "debug", skip(self))]
    async fn capture_raw_frame(&self) -> Result<()> {
        // The capture pipeline stays in Playing between snapshots.
        // A pad blocker on the queue's src pad prevents data from
        // reaching the decoder when idle, so CPU usage is negligible.
        // Cycling through Null would lose the proxy's sticky events
        // (stream-start, caps, segment) which cannot be reliably
        // restored, causing "data flow before stream-start" warnings
        // and segfaults in avdec_h264.
        if self.pipeline.current_state() != gst::State::Playing {
            self.pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| anyhow!("Failed to set capture pipeline to Playing: {error:?}"))?;
            self.pipeline
                .state(gst::ClockTime::from_seconds(10))
                .0
                .map_err(|error| anyhow!("Capture pipeline failed to reach Playing: {error:?}"))?;
        }

        // Subscribe BEFORE unblocking to guarantee we receive the notification
        let mut receiver = self.flat_samples_sender.subscribe();

        // Unblock the queue's src pad to allow data to flow to the decoder
        if let Some(blocker) = self.pad_blocker.lock().unwrap().take()
            && let Some(queue_src_pad) = self.queue.static_pad("src") {
                queue_src_pad.remove_probe(blocker);
            }

        // Request an immediate keyframe from the upstream encoder so the
        // decoder can produce a frame without waiting for the next natural
        // keyframe (which may be many seconds away with x264enc defaults).
        if let Some(queue_src_pad) = self.queue.static_pad("src") {
            let event = gst_video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            queue_src_pad.send_event(event);
        }

        let result =
            tokio::time::timeout(tokio::time::Duration::from_secs(15), receiver.recv()).await;

        result??.map_err(anyhow::Error::msg)
    }

    /// Read the raw frame from `appsink`'s `last-sample`, configure the
    /// encoding pipeline for the given settings, push the frame through, and
    /// return the resulting JPEG bytes.
    #[instrument(level = "debug", skip(self))]
    async fn encode_raw_frame(&self, settings: &ThumbnailSettings) -> Result<Vec<u8>> {
        let sample: gst::Sample = self
            .appsink
            .property::<Option<gst::Sample>>("last-sample")
            .ok_or_else(|| anyhow!("No raw frame available in last-sample"))?;
        let caps = sample
            .caps()
            .ok_or_else(|| anyhow!("Raw sample has no caps"))?
            .to_owned();
        let buffer = sample
            .buffer()
            .ok_or_else(|| anyhow!("Raw sample has no buffer"))?;

        // Configure encoding elements for this request's settings
        self.jpegenc
            .set_property("quality", settings.quality as i32);

        if let Some(target_h) = settings.target_height {
            let aspect_ratio = self.source_width as f64 / self.source_height as f64;
            let target_w = (target_h as f64 * aspect_ratio) as u32;
            // Ensure even dimensions for YUV format compatibility
            let target_w = target_w + (target_w % 2);
            let target_h = target_h + (target_h % 2);

            let scale_caps = gst::Caps::builder("video/x-raw")
                .field("width", target_w as i32)
                .field("height", target_h as i32)
                .build();
            self.scale_capsfilter.set_property("caps", &scale_caps);
        } else {
            self.scale_capsfilter
                .set_property("caps", gst::Caps::new_any());
        }

        // Set appsrc caps to match the raw frame format
        self.encode_appsrc.set_caps(Some(&caps));

        // Ensure encoding pipeline is Playing
        if self.encode_pipeline.current_state() != gst::State::Playing {
            self.encode_pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| anyhow!("Failed to set encode pipeline to Playing: {error:?}"))?;
            self.encode_pipeline
                .state(gst::ClockTime::from_seconds(10))
                .0
                .map_err(|error| anyhow!("Encode pipeline failed to reach Playing: {error:?}"))?;
        }

        // Subscribe BEFORE pushing to guarantee we receive the result
        let mut receiver = self.encode_sender.subscribe();

        let buffer_copy = buffer
            .copy_deep()
            .map_err(|e| anyhow!("Failed to deep-copy raw buffer: {e}"))?;
        self.encode_appsrc
            .push_buffer(buffer_copy)
            .map_err(|err| anyhow!("Failed to push buffer into encode appsrc: {err:?}"))?;

        let result =
            tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver.recv()).await;

        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }

        result??.map_err(anyhow::Error::msg)
    }

    /// Produce a JPEG thumbnail with the requested settings.
    ///
    /// Uses a two-level stampede-proof cache:
    ///  - Level 1: raw decoded frame (GStreamer's `last-sample` on `appsink`)
    ///  - Level 2: JPEG per `ThumbnailSettings`
    ///
    /// Both levels use double-checked locking so that concurrent requests for
    /// the same entry coalesce into a single production.
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

        // Level 2 quick check: return cached JPEG if still fresh
        {
            let cache = self
                .thumbnails
                .lock()
                .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))?;
            if let Some(jpeg) = cache.try_get(&settings)? {
                return Ok(jpeg);
            }
        }

        // Level 1: ensure the raw frame is fresh (stampede-proof)
        {
            let is_fresh = self
                .last_capture_instant
                .lock()
                .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))?
                .map(|instant| instant.elapsed() < std::time::Duration::from_secs(1))
                .unwrap_or(false);

            if !is_fresh {
                let _guard = self.raw_capture_mutex.lock().await;

                // Double-check after acquiring the serializer
                let still_stale = self
                    .last_capture_instant
                    .lock()
                    .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))?
                    .map(|instant| instant.elapsed() >= std::time::Duration::from_secs(1))
                    .unwrap_or(true);

                if still_stale {
                    self.capture_raw_frame().await?;

                    *self
                        .last_capture_instant
                        .lock()
                        .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))? =
                        Some(std::time::Instant::now());

                    // Invalidate JPEG cache since the underlying raw frame changed
                    self.thumbnails
                        .lock()
                        .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))?
                        .clear();
                }
            }
        }

        // Level 2: encode with stampede prevention
        {
            let _guard = self.encode_mutex.lock().await;

            // Double-check after acquiring the serializer
            {
                let cache = self
                    .thumbnails
                    .lock()
                    .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))?;
                if let Some(jpeg) = cache.try_get(&settings)? {
                    return Ok(jpeg);
                }
            }

            let jpeg_bytes = self.encode_raw_frame(&settings).await?;

            {
                let mut cache = self
                    .thumbnails
                    .lock()
                    .map_err(|e| anyhow!("Failed locking a Mutex. Reason: {e}"))?;
                if let Err(error) = cache.try_set(&settings, jpeg_bytes.clone()) {
                    error!("Failed setting cached thumbnail. Reason: {error:?}");
                }
            }

            Ok(jpeg_bytes)
        }
    }
}
