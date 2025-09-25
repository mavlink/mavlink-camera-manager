use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Context, Error, Result};
use gst::prelude::*;
use gst_video::VideoFrameExt;
use image::FlatSamples;
use tracing::*;

use crate::{stream::pipeline::runner::PipelineRunner, video::types::VideoEncodeType};

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
    _transcoding_elements: Vec<gst::Element>,
    appsink: gst_app::AppSink,
    tee_src_pad: Option<gst::Pad>,
    flat_samples_sender: tokio::sync::broadcast::Sender<ClonableResult<FlatSamples<Vec<u8>>>>,
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
    #[instrument(level = "debug")]
    pub fn try_new(sink_id: Arc<uuid::Uuid>, encoding: VideoEncodeType) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("silent", true)
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
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

        // Depending of the sources' format we need different elements to transform it into a raw format
        let mut _transcoding_elements: Vec<gst::Element> = Default::default();
        match encoding {
            VideoEncodeType::H264 => {
                // For h264, we need to filter-out unwanted non-key frames here, before decoding it.
                let filter = gst::ElementFactory::make("identity")
                    .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                    .property("sync", false)
                    .build()?;
                let decoder = gst::ElementFactory::make("avdec_h264")
                    .property_from_str("lowres", "2") // (0) is 'full'; (1) is '1/2-size'; (2) is '1/4-size'
                    .build()?;
                decoder.has_property("discard-corrupted-frames", None).then(|| decoder.set_property("discard-corrupted-frames", true));
                _transcoding_elements.push(filter);
                _transcoding_elements.push(decoder);
            }
            VideoEncodeType::H265 => {
                // For h265, we need to filter-out unwanted non-key frames here, before decoding it.
                let filter = gst::ElementFactory::make("identity")
                .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                .property("sync", false)
                .build()?;
                let decoder = gst::ElementFactory::make("avdec_h265")
                    .property_from_str("lowres", "2") // (0) is 'full'; (1) is '1/2-size'; (2) is '1/4-size'
                    .build()?;
                decoder.has_property("discard-corrupted-frames", None).then(|| decoder.set_property("discard-corrupted-frames", true));
                decoder.has_property("std-compliance", None).then(|| decoder.set_property_from_str("std-compliance", "normal"));
                _transcoding_elements.push(filter);
                _transcoding_elements.push(decoder);
            }
            VideoEncodeType::Mjpg => {
                let decoder = gst::ElementFactory::make("jpegdec").build()?;
                decoder.has_property("discard-corrupted-frames", None).then(|| decoder.set_property("discard-corrupted-frames", true));
                _transcoding_elements.push(decoder);
            }
            VideoEncodeType::Rgb => {}
            VideoEncodeType::Yuyv => {}
            _ => return Err(anyhow!("Unsupported video encoding for ImageSink: {encoding:?}. The supported are: H264, MJPG and YUYV")),
        };

        let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
        _transcoding_elements.push(videoconvert);

        // We want RGB format
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", gst_video::VideoFormat::Rgbx.to_str())
            .build();

        let pad_blocker: Arc<Mutex<Option<gst::PadProbeId>>> = Default::default();
        let pad_blocker_clone = pad_blocker.clone();
        let queue_src_pad = queue.static_pad("src").expect("No src pad found on Queue");

        // To get data out of the callback, we'll be using this arc mutex
        let (sender, _) = tokio::sync::broadcast::channel(1);
        let flat_samples_sender = sender.clone();
        let mut pending = false;

        // The appsink will then call those handlers, as soon as data is available.
        let appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            // Add a handler to the "new-sample" signal.
            .new_sample(move |appsink| {
                // Only process if requested
                if sender.receiver_count() == 0 || pending {
                    return Ok(gst::FlowSuccess::Ok);
                }
                debug!("Starting a snapshot");
                pending = true;

                // Pull the sample in question out of the appsink's buffer
                let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or_else(|| {
                    let reason = "Failed to get buffer from appsink";
                    gst::element_error!(appsink, gst::ResourceError::Failed, ("{reason:?}"));

                    let _ = sender.send(Err(Arc::new(anyhow!(reason))));
                    pending = false;

                    gst::FlowError::Error
                })?;

                // Drop non-key frames
                if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                    let _ = sender.send(Err(Arc::new(anyhow!("Not a valid frame"))));
                    pending = false;

                    return Ok(gst::FlowSuccess::Ok);
                }

                // Got a valid frame, block any further frame until next request
                if let Some(old_blocker) = queue_src_pad
                    .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                        gst::PadProbeReturn::Ok
                    })
                    .and_then(|blocker| pad_blocker_clone.lock().unwrap().replace(blocker))
                {
                    queue_src_pad.remove_probe(old_blocker);
                }

                let caps = sample.caps().expect("Sample without caps");
                let info = gst_video::VideoInfo::from_caps(caps).expect("Failed to parse caps");

                // At this point, buffer is only a reference to an existing memory region somewhere.
                // When we want to access its content, we have to map it while requesting the required
                // mode of access (read, read/write).
                // This type of abstraction is necessary, because the buffer in question might not be
                // on the machine's main memory itself, but rather in the GPU's memory.
                // So mapping the buffer makes the underlying memory region accessible to us.
                // See: https://gstreamer.freedesktop.org/documentation/plugin-development/advanced/allocation.html
                let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                    .map_err(|_| {
                        let reason = "Failed to map buffer readable";
                        gst::element_error!(appsink, gst::ResourceError::Failed, ("{reason:?}"));

                        let _ = sender.send(Err(Arc::new(anyhow!(reason))));
                        pending = false;

                        gst::FlowError::Error
                    })?;

                // Create a FlatSamples around the borrowed video frame data from GStreamer with
                // the correct stride as provided by GStreamer.
                let frame = image::FlatSamples::<Vec<u8>> {
                    samples: frame.plane_data(0).unwrap().to_vec(),
                    layout: image::flat::SampleLayout {
                        // RGB
                        channels: 3,
                        // 1 byte from component to component
                        channel_stride: 1,
                        width: frame.width(),
                        // 4 byte from pixel to pixel
                        width_stride: 4,
                        height: frame.height(),
                        // stride from line to line
                        height_stride: frame.plane_stride()[0] as usize,
                    },
                    color_hint: Some(image::ColorType::Rgb8),
                };

                // Send the data
                let _ = sender.send(Ok(frame));
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
            .caps(&caps)
            .callbacks(appsink_callbacks)
            .build();

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

        let pipeline_runner = PipelineRunner::try_new(&pipeline, &sink_id, true)?;

        // Start the pipeline in Pause, because we want to wait the snapshot
        if let Err(state_err) = pipeline.set_state(gst::State::Paused) {
            return Err(anyhow!(
                "Failed pausing ImageSink's pipeline: {state_err:#?}"
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
            tee_src_pad: Default::default(),
            flat_samples_sender,
            pad_blocker,
            pipeline_runner,
            thumbnails: Default::default(),
        })
    }

    #[instrument(level = "debug", skip(self))]
    async fn try_get_flat_sample(&self) -> Result<FlatSamples<Vec<u8>>> {
        // Play the pipeline if it's not playing yet.
        // Here we can ignore the result because we have a timeout when waiting for the snapshot
        if self.pipeline.current_state() != gst::State::Playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
        }

        // Unblock the data from entering the ProxySink
        if let Some(blocker) = self.pad_blocker.lock().unwrap().take() {
            self.queue
                .static_pad("src")
                .expect("No src pad found on Queue")
                .remove_probe(blocker);
        }

        // Trigger the snapshot
        let mut receiver = self.flat_samples_sender.subscribe();

        // Wait for the snapshot to be taken, with a timeout
        // Here we'd have the raw snapshot, we just need to convert it to the final format/size
        tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver.recv())
            .await??
            .map_err(|e| anyhow!(e.to_string()))
    }

    #[instrument(level = "debug", skip(self, flat_sample))]
    async fn try_process_sample(
        &self,
        flat_sample: FlatSamples<Vec<u8>>,
        info: gst_video::VideoInfo,
        settings: ThumbnailSettings,
    ) -> Result<Vec<u8>> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Vec<u8>>>();
        let (quality, target_height) = (settings.quality, settings.target_height);
        tokio::task::spawn(async move {
            // Calculate a target width/height that keeps the display aspect ratio while having
            // a height of the given target_height (eg 240 pixels)
            let display_aspect_ratio = (flat_sample.layout.width as f64
                * info.par().numer() as f64)
                / (flat_sample.layout.height as f64 * info.par().denom() as f64);
            let target_height = target_height.unwrap_or(flat_sample.layout.height);
            let target_width = (target_height as f64 * display_aspect_ratio) as u32;

            // Scale image to our target dimensions
            let image_view = match flat_sample.as_view::<image::Rgb<u8>>() {
                Ok(image_view) => image_view,
                Err(error) => {
                    let _ = tx.send(Err(anyhow!("Failed creating image view. Reason: {error}")));
                    return;
                }
            };
            let img_buf = image::imageops::thumbnail(&image_view, target_width, target_height);

            // Transmute it to our output buffer, which represents the image itself in the chosen file format
            let mut buffer = std::io::Cursor::new(Vec::new());
            if let Err(error) = image::DynamicImage::ImageRgb8(img_buf)
                .write_to(&mut buffer, image::ImageOutputFormat::Jpeg(quality))
            {
                let _ = tx.send(Err(anyhow!("Failed creating image. Reason: {error}")));
                return;
            }

            let _ = tx.send(Ok(buffer.into_inner()));
        });

        let thumbnail = tokio::time::timeout(tokio::time::Duration::from_secs(2), rx).await???;

        {
            let mut thumbnails = match self.thumbnails.lock() {
                Ok(guard) => guard,
                Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
            };

            if let Err(error) = thumbnails.try_set(&settings, thumbnail.clone()) {
                error!("Failed setting cached thumbnail. Reason: {error:?}");
            }
        }

        Ok(thumbnail)
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

        // If cache doesn't hit, produce a new one
        let flat_sample = self.try_get_flat_sample().await?;

        let caps = &self
            .appsink
            .static_pad("sink")
            .expect("No static sink pad found on capsfilter")
            .current_caps()
            .context("Failed to get caps from capsfilter sink pad")?;
        let info = gst_video::VideoInfo::from_caps(caps).context("Failed to parse caps")?;

        self.try_process_sample(flat_sample, info, settings).await
    }
}
