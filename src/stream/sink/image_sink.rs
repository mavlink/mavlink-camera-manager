use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};

use tracing::*;

use image::FlatSamples;

use gst::prelude::*;

use super::SinkInterface;
use crate::{stream::pipeline::PIPELINE_SINK_TEE_NAME, video::types::VideoEncodeType};

#[derive(Debug)]
pub struct ImageSink {
    sink_id: uuid::Uuid,
    queue: gst::Element,
    transcoding_elements: Vec<gst::Element>,
    appsink: gst_app::AppSink,
    tee_src_pad: Option<gst::Pad>,
    flat_samples: Arc<Mutex<Option<FlatSamples<Vec<u8>>>>>,
}
impl SinkInterface for ImageSink {
    #[instrument(level = "debug", skip(self))]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &uuid::Uuid,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        let sink_id = &self.get_id();

        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from ImageSink {sink_id} has already been configured"
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        // Add Sink elements to the Pipeline
        let mut elements = vec![&self.queue];
        elements.extend(
            self.transcoding_elements
                .iter()
                .collect::<Vec<&gst::Element>>(),
        );
        elements.push(self.appsink.upcast_ref());
        let elements = &elements;
        if let Err(error) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding ImageSink {sink_id} elements to Pipeline {pipeline_id}. Reason: {error:?}"
            ));
        }

        // Link Sink elements
        if let Err(error) = gst::Element::link_many(elements) {
            if let Err(error) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from Pipeline {pipeline_id}. Reason: {error:?}")
            };
            return Err(anyhow!(
                "Failed linking ImageSink {sink_id} elements from Pipeline {pipeline_id:?}. Reason: {error:?}"
            ));
        }

        // Link the new Tee's src pad to the queue's sink pad
        let queue_sink_pad = &self
            .queue
            .static_pad("sink")
            .expect("No src pad found on Queue");
        if let Err(error) = tee_src_pad.link(queue_sink_pad) {
            if let Err(error) = pipeline.remove_many(elements) {
                error!("Failed removing elements from Pipeline {pipeline_id:?}. Reason: {error:?}");
            }
            gst::Element::unlink_many(elements);
            return Err(anyhow!(
                "Failed linking ImageSink's queue sink to Tee's src pad. Reason: {error:?}"
            ));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &uuid::Uuid) -> Result<()> {
        let sink_id = self.get_id();

        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink sink {sink_id} from pipeline {pipeline_id} without a Tee src pad.");
            return Ok(());
        };

        let queue_sink_pad = self
            .queue
            .static_pad("sink")
            .expect("No sink pad found on Queue");
        if let Err(error) = tee_src_pad.unlink(&queue_sink_pad) {
            return Err(anyhow!(
                "Failed unlinking Sink {sink_id} from Tee's source pad. Reason: {error:?}"
            ));
        }
        drop(queue_sink_pad);

        let mut elements = vec![&self.queue];
        elements.extend(
            self.transcoding_elements
                .iter()
                .collect::<Vec<&gst::Element>>(),
        );
        elements.push(self.appsink.upcast_ref());
        let elements = &elements;
        if let Err(error) = pipeline.remove_many(elements) {
            return Err(anyhow!(
                "Failed removing ImageSink {sink_id} elements from Pipeline {pipeline_id}. Reason: {error:?}"
            ));
        }

        if let Err(error) = self.queue.set_state(gst::State::Null) {
            return Err(anyhow!(
                "Failed to set queue from sink {sink_id} state to NULL. Reason: {error:#?}"
            ));
        }

        let sink_name = format!("{PIPELINE_SINK_TEE_NAME}-{pipeline_id}");
        let tee = pipeline
            .by_name(&sink_name)
            .context(format!("no element named {sink_name:#?}"))?;
        if let Err(error) = tee.remove_pad(tee_src_pad) {
            return Err(anyhow!(
                "Failed removing Tee's source pad. Reason: {error:?}"
            ));
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn get_id(&self) -> uuid::Uuid {
        self.sink_id
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available. Reason: Image Sink doesn't provide endpoints"
        ))
    }
}

impl ImageSink {
    #[instrument(level = "debug")]
    pub fn try_new(id: uuid::Uuid, encoding: VideoEncodeType) -> Result<Self> {
        let queue = gst::ElementFactory::make("queue")
            .property_from_str("leaky", "downstream") // Throw away any data
            .property("flush-on-eos", true)
            .property("max-size-buffers", 0u32) // Disable buffers
            .build()?;

        let mut transcoding_elements: Vec<gst::Element> = Default::default();

        match encoding {
            VideoEncodeType::H264 => {
                let depayloader = gst::ElementFactory::make("rtph264depay").build()?;
                let parser = gst::ElementFactory::make("h264parse").build()?;
                // For h264, we need to filter-out unwanted non-key frames here, before decoding it.
                let filter = gst::ElementFactory::make("identity")
                .property("drop-buffer-flags", gst::BufferFlags::DELTA_UNIT)
                .property("sync", false)
                .build()?;
                let decoder = gst::ElementFactory::make("avdec_h264")
                    .property("discard-corrupted-frames", true)
                    .build()?;
                transcoding_elements.push(depayloader);
                transcoding_elements.push(parser);
                transcoding_elements.push(filter);
                transcoding_elements.push(decoder);
            }
            VideoEncodeType::Mjpg => {
                let depayloader = gst::ElementFactory::make("rtpjpegdepay").build()?;
                let parser = gst::ElementFactory::make("jpegparse").build()?;
                let decoder = gst::ElementFactory::make("jpegdec")
                    .property("discard-corrupted-frames", true)
                    .build()?;
                transcoding_elements.push(depayloader);
                transcoding_elements.push(parser);
                transcoding_elements.push(decoder);
            }
            VideoEncodeType::Yuyv => {
                let depayloader = gst::ElementFactory::make("rtpvrawdepay").build()?;
                transcoding_elements.push(depayloader);
            }
            _ => return Err(anyhow!("Unsupported video encoding for ImageSink: {encoding:?}. The supported are: H264, MJPG and YUYV")),
        };

        let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
        transcoding_elements.push(videoconvert);

        let caps = gst::Caps::builder("video/x-raw")
            .field("format", gst_video::VideoFormat::Rgbx.to_str())
            .build();

        // To get data out of the callback, we'll be using this arc mutex
        let flat_samples: Arc<Mutex<Option<image::FlatSamples<Vec<u8>>>>> = Default::default();
        let flat_samples_cloned = flat_samples.clone();

        // The appsink will then call those handlers, as soon as data is available.
        let appsink_callbacks = gst_app::AppSinkCallbacks::builder()
            // Add a handler to the "new-sample" signal.
            .new_sample(move |appsink| {
                // Pull the sample in question out of the appsink's buffer.
                let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or_else(|| {
                    gst::element_error!(
                        appsink,
                        gst::ResourceError::Failed,
                        ("Failed to get buffer from appsink")
                    );

                    gst::FlowError::Error
                })?;

                // Drop non-key frames
                if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                    return Ok(gst::FlowSuccess::Ok);
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
                        gst::element_error!(
                            appsink,
                            gst::ResourceError::Failed,
                            ("Failed to map buffer readable")
                        );

                        gst::FlowError::Error
                    })?;

                // Create a FlatSamples around the borrowed video frame data from GStreamer with
                // the correct stride as provided by GStreamer.
                flat_samples_cloned
                    .lock()
                    .unwrap()
                    .replace(image::FlatSamples::<Vec<u8>> {
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
                    });

                std::thread::sleep(std::time::Duration::from_secs(1)); // This defines the interval between shots
                Ok(gst::FlowSuccess::Ok)
            })
            .build();

        let appsink = gst_app::AppSink::builder()
            .name(&format!("AppSink-{id}"))
            .sync(false)
            .caps(&caps)
            .callbacks(appsink_callbacks)
            .build();

        Ok(Self {
            sink_id: id,
            queue,
            transcoding_elements,
            appsink,
            tee_src_pad: Default::default(),
            flat_samples,
        })
    }

    #[instrument(level = "debug", skip(self))]
    pub fn make_jpeg_thumbnail_from_last_frame(
        &self,
        quality: u8,
        target_height: Option<u32>,
    ) -> Result<Vec<u8>> {
        let caps = &self
            .appsink
            .static_pad("sink")
            .expect("No static sink pad found on capsfilter")
            .current_caps()
            .context("Failed to get caps from capsfilter sink pad")?;
        let info = gst_video::VideoInfo::from_caps(caps).context("Failed to parse caps")?;

        let Some(flat_samples) = self.flat_samples.lock().unwrap().clone() else {
            return Err(anyhow!("Thumbnail not available yet"))
        };

        // Calculate a target width/height that keeps the display aspect ratio while having
        // a height of the given target_height (eg 240 pixels)
        let display_aspect_ratio = (flat_samples.layout.width as f64 * info.par().numer() as f64)
            / (flat_samples.layout.height as f64 * info.par().denom() as f64);
        let target_height = target_height.unwrap_or(flat_samples.layout.height);
        let target_width = (target_height as f64 * display_aspect_ratio) as u32;

        // Scale image to our target dimensions
        let img_buf = image::imageops::thumbnail(
            &flat_samples
                .as_view::<image::Rgb<u8>>()
                .expect("couldn't create image view"),
            target_width,
            target_height,
        );

        // Transmute it to our output buffer, which represents the image itself in the chosen file format
        let mut buffer = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img_buf)
            .write_to(&mut buffer, image::ImageOutputFormat::Jpeg(quality))
            .unwrap();

        Ok(buffer.into_inner())
    }
}
