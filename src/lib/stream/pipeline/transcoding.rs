use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tracing::warn;

use crate::{
    controls::gst_element_controls::set_property_from_api,
    stream::{
        gst::{encoding::CompressedEncoding, utils::try_set_property},
        types::{ManualTranscodingConfig, PropertyValue},
    },
    video::types::{FrameInterval, VideoEncodeType},
};

use super::{
    PIPELINE_FILTER_NAME, PIPELINE_RAW_TEE_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME,
};

pub struct ManualTranscodingPipeline {
    pub encoding: Option<&'static dyn CompressedEncoding>,
    pub source_encode: VideoEncodeType,
    pub width: u32,
    pub height: u32,
    pub manual_config: ManualTranscodingConfig,
}

impl ManualTranscodingPipeline {
    pub fn build_pipeline(
        &self,
        device_path: &str,
        pipeline_id: &Arc<uuid::Uuid>,
        source_factory_name: Option<&str>,
        frame_interval: Option<&FrameInterval>,
    ) -> Result<gst::Pipeline> {
        if matches!(
            self.source_encode,
            VideoEncodeType::Nv12 | VideoEncodeType::Yuyv | VideoEncodeType::Rgb
        ) {
            return self.build_raw_pipeline(device_path, pipeline_id, source_factory_name);
        }

        let frame_interval = frame_interval
            .context("Compressed manual transcoding requires frame interval for source caps")?;
        if self.encoding.is_none() {
            return self.build_decode_pipeline(pipeline_id, source_factory_name, frame_interval);
        }
        self.build_compressed_pipeline(
            device_path,
            pipeline_id,
            source_factory_name,
            frame_interval,
        )
    }

    fn build_raw_pipeline(
        &self,
        device_path: &str,
        pipeline_id: &Arc<uuid::Uuid>,
        source_factory_name: Option<&str>,
    ) -> Result<gst::Pipeline> {
        let encoding = self.compressed_encoding()?;
        let raw_format = raw_caps_format(&self.source_encode)?;
        let factory_name = encoder_factory_name(encoding, &self.manual_config);
        let Some(factory) = gst::ElementFactory::find(&factory_name) else {
            return Err(anyhow!(
                "GStreamer encoder factory {factory_name} is not available"
            ));
        };
        let caps = gst::Caps::builder(encoding.caps_mime()).build();
        if !factory.can_src_any_caps(caps.as_ref()) {
            return Err(anyhow!(
                "GStreamer factory {factory_name} does not produce {}",
                encoding.encode_key()
            ));
        }
        crate::stream::gst::utils::encoder_factory_can_encode(encoding, &factory_name)
            .with_context(|| {
                format!(
                    "GStreamer encoder {factory_name} cannot encode {}",
                    encoding.encode_key()
                )
            })?;

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let raw_tee_name = format!("{PIPELINE_RAW_TEE_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let pipeline = gst::Pipeline::new();

        let raw_capsfilter = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", raw_format)
                    .field("width", self.width as i32)
                    .field("height", self.height as i32)
                    .build(),
            )
            .build()
            .context("Failed to create raw capsfilter")?;

        let raw_tee = gst::ElementFactory::make("tee")
            .name(raw_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create raw tee")?;

        let queue = gst::ElementFactory::make("queue")
            .build()
            .context("Failed to create queue")?;
        queue.set_property_from_str("leaky", "downstream");
        queue.set_property("max-size-buffers", 2u32);
        queue.set_property("max-size-time", gst::ClockTime::ZERO);
        queue.set_property("max-size-bytes", 0u32);

        let videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .context("Failed to create videoconvert")?;

        let encoder_input_capsfilter = mjpg_encoder_input_capsfilter(encoding)?;

        let encoder = gst::ElementFactory::make(factory_name.as_str())
            .name("encoder")
            .build()
            .with_context(|| format!("Failed to create encoder {factory_name}"))?;

        let parser = match encoding.optional_parser_factory() {
            Some(parser_factory) => {
                let parser = gst::ElementFactory::make(parser_factory)
                    .build()
                    .with_context(|| format!("Failed to create parser {parser_factory}"))?;
                encoding.configure_parser_element(&parser);
                Some(parser)
            }
            None => None,
        };

        let compressed_capsfilter = gst::ElementFactory::make("capsfilter")
            .name(filter_name.as_str())
            .property("caps", encoding.compressed_caps(self.width, self.height))
            .build()
            .context("Failed to create compressed capsfilter")?;

        let video_tee = gst::ElementFactory::make("tee")
            .name(video_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create video tee")?;

        let pay = gst::ElementFactory::make(encoding.pay_factory_name())
            .build()
            .with_context(|| {
                format!("Failed to create payloader {}", encoding.pay_factory_name())
            })?;
        encoding.configure_pay_element(&pay);

        let rtp_tee = gst::ElementFactory::make("tee")
            .name(rtp_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create RTP tee")?;

        let mut chain: Vec<&gst::Element> = vec![&raw_capsfilter, &raw_tee, &queue, &videoconvert];
        if let Some(encoder_input_capsfilter) = &encoder_input_capsfilter {
            chain.push(encoder_input_capsfilter);
        }
        chain.push(&encoder);
        if let Some(parser) = &parser {
            chain.push(parser);
        }
        chain.extend([&compressed_capsfilter, &video_tee, &pay, &rtp_tee]);

        pipeline
            .add_many(&chain)
            .context("Failed to add manual transcoding elements")?;
        gst::Element::link_many(&chain).context("Failed to link manual transcoding chain")?;

        // Source last: set camera-name before add, link only source -> raw_capsfilter.
        let source_factory_name = source_factory_name.unwrap_or("libcamerasrc");
        let source = gst::ElementFactory::make(source_factory_name)
            .name("source")
            .build()
            .with_context(|| format!("Failed to create source {source_factory_name}"))?;
        if source_factory_name == "libcamerasrc" {
            source.set_property("camera-name", device_path);
        }
        pipeline
            .add(&source)
            .context("Failed to add source element")?;
        source
            .link(&raw_capsfilter)
            .context("Failed to link source to raw capsfilter")?;

        Ok(pipeline)
    }

    fn build_decode_pipeline(
        &self,
        pipeline_id: &Arc<uuid::Uuid>,
        source_factory_name: Option<&str>,
        frame_interval: &FrameInterval,
    ) -> Result<gst::Pipeline> {
        let decoder_factory_name = decoder_factory_name(&self.source_encode, &self.manual_config)?;
        let Some(decoder_factory) = gst::ElementFactory::find(&decoder_factory_name) else {
            return Err(anyhow!(
                "GStreamer decoder factory {decoder_factory_name} is not available"
            ));
        };
        let source_caps =
            compressed_source_caps(&self.source_encode, self.width, self.height, frame_interval)?;
        if !decoder_factory.can_sink_any_caps(source_caps.as_ref()) {
            return Err(anyhow!(
                "GStreamer factory {decoder_factory_name} cannot decode {:?}",
                self.source_encode
            ));
        }

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let pipeline = gst::Pipeline::new();

        let source_capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", source_caps)
            .build()
            .context("Failed to create source capsfilter")?;

        let decoder = gst::ElementFactory::make(decoder_factory_name.as_str())
            .name("decoder")
            .build()
            .with_context(|| format!("Failed to create decoder {decoder_factory_name}"))?;

        let queue = gst::ElementFactory::make("queue")
            .build()
            .context("Failed to create queue")?;
        queue.set_property_from_str("leaky", "downstream");
        queue.set_property("max-size-buffers", 2u32);
        queue.set_property("max-size-time", gst::ClockTime::ZERO);
        queue.set_property("max-size-bytes", 0u32);

        let videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .context("Failed to create videoconvert")?;

        let framerate = gst::Fraction::new(
            frame_interval.denominator as i32,
            frame_interval.numerator as i32,
        );
        let raw_capsfilter = gst::ElementFactory::make("capsfilter")
            .name(filter_name.as_str())
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", "I420")
                    .field("width", self.width as i32)
                    .field("height", self.height as i32)
                    .field("framerate", framerate)
                    .build(),
            )
            .build()
            .context("Failed to create raw capsfilter")?;

        let video_tee = gst::ElementFactory::make("tee")
            .name(video_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create video tee")?;

        let pay = gst::ElementFactory::make("rtpvrawpay")
            .property("pt", 96u32)
            .build()
            .context("Failed to create rtpvrawpay")?;

        let rtp_tee = gst::ElementFactory::make("tee")
            .name(rtp_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create RTP tee")?;

        pipeline
            .add_many([
                &source_capsfilter,
                &decoder,
                &queue,
                &videoconvert,
                &raw_capsfilter,
                &video_tee,
                &pay,
                &rtp_tee,
            ])
            .context("Failed to add manual decode elements")?;
        gst::Element::link_many([
            &source_capsfilter,
            &decoder,
            &queue,
            &videoconvert,
            &raw_capsfilter,
            &video_tee,
            &pay,
            &rtp_tee,
        ])
        .context("Failed to link manual decode chain")?;

        if let Some(source_factory_name) = source_factory_name {
            add_compressed_source(&pipeline, source_factory_name, &source_capsfilter)?;
        }

        Ok(pipeline)
    }

    fn build_compressed_pipeline(
        &self,
        _device_path: &str,
        pipeline_id: &Arc<uuid::Uuid>,
        source_factory_name: Option<&str>,
        frame_interval: &FrameInterval,
    ) -> Result<gst::Pipeline> {
        let decoder_factory_name = decoder_factory_name(&self.source_encode, &self.manual_config)?;
        let Some(decoder_factory) = gst::ElementFactory::find(&decoder_factory_name) else {
            return Err(anyhow!(
                "GStreamer decoder factory {decoder_factory_name} is not available"
            ));
        };
        let source_caps =
            compressed_source_caps(&self.source_encode, self.width, self.height, frame_interval)?;
        if !decoder_factory.can_sink_any_caps(source_caps.as_ref()) {
            return Err(anyhow!(
                "GStreamer factory {decoder_factory_name} cannot decode {:?}",
                self.source_encode
            ));
        }

        let encoding = self.compressed_encoding()?;
        let encoder_factory_name = encoder_factory_name(encoding, &self.manual_config);
        let Some(encoder_factory) = gst::ElementFactory::find(&encoder_factory_name) else {
            return Err(anyhow!(
                "GStreamer encoder factory {encoder_factory_name} is not available"
            ));
        };
        let sink_caps = gst::Caps::builder(encoding.caps_mime()).build();
        if !encoder_factory.can_src_any_caps(sink_caps.as_ref()) {
            return Err(anyhow!(
                "GStreamer factory {encoder_factory_name} does not produce {}",
                encoding.encode_key()
            ));
        }
        crate::stream::gst::utils::encoder_factory_can_encode(encoding, &encoder_factory_name)
            .with_context(|| {
                format!(
                    "GStreamer encoder {encoder_factory_name} cannot encode {}",
                    encoding.encode_key()
                )
            })?;

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let pipeline = gst::Pipeline::new();

        let source_capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", source_caps)
            .build()
            .context("Failed to create source capsfilter")?;

        let decoder = gst::ElementFactory::make(decoder_factory_name.as_str())
            .name("decoder")
            .build()
            .with_context(|| format!("Failed to create decoder {decoder_factory_name}"))?;

        let queue = gst::ElementFactory::make("queue")
            .build()
            .context("Failed to create queue")?;
        queue.set_property_from_str("leaky", "downstream");
        queue.set_property("max-size-buffers", 2u32);
        queue.set_property("max-size-time", gst::ClockTime::ZERO);
        queue.set_property("max-size-bytes", 0u32);

        let videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .context("Failed to create videoconvert")?;
        let encoder_input_capsfilter = mjpg_encoder_input_capsfilter(encoding)?;

        let encoder = gst::ElementFactory::make(encoder_factory_name.as_str())
            .name("encoder")
            .build()
            .with_context(|| format!("Failed to create encoder {encoder_factory_name}"))?;

        let parser = match encoding.optional_parser_factory() {
            Some(parser_factory) => {
                let parser = gst::ElementFactory::make(parser_factory)
                    .build()
                    .with_context(|| format!("Failed to create parser {parser_factory}"))?;
                encoding.configure_parser_element(&parser);
                Some(parser)
            }
            None => None,
        };

        let compressed_capsfilter = gst::ElementFactory::make("capsfilter")
            .name(filter_name.as_str())
            .property("caps", encoding.compressed_caps(self.width, self.height))
            .build()
            .context("Failed to create compressed capsfilter")?;

        let video_tee = gst::ElementFactory::make("tee")
            .name(video_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create video tee")?;

        let pay = gst::ElementFactory::make(encoding.pay_factory_name())
            .build()
            .with_context(|| {
                format!("Failed to create payloader {}", encoding.pay_factory_name())
            })?;
        encoding.configure_pay_element(&pay);

        let rtp_tee = gst::ElementFactory::make("tee")
            .name(rtp_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create RTP tee")?;

        let mut chain: Vec<&gst::Element> =
            vec![&source_capsfilter, &decoder, &queue, &videoconvert];
        if let Some(encoder_input_capsfilter) = &encoder_input_capsfilter {
            chain.push(encoder_input_capsfilter);
        }
        chain.push(&encoder);
        if let Some(parser) = &parser {
            chain.push(parser);
        }
        chain.extend([&compressed_capsfilter, &video_tee, &pay, &rtp_tee]);

        pipeline
            .add_many(&chain)
            .context("Failed to add compressed manual transcoding elements")?;
        gst::Element::link_many(&chain)
            .context("Failed to link compressed manual transcoding chain")?;

        if let Some(source_factory_name) = source_factory_name {
            add_compressed_source(&pipeline, source_factory_name, &source_capsfilter)?;
        }

        Ok(pipeline)
    }

    pub fn apply_runtime_properties(&self, pipeline: &gst::Pipeline) -> Result<()> {
        if let Some(encoder) = pipeline.by_name("encoder") {
            let encoding = self.compressed_encoding()?;
            for (property_name, property_value) in
                startup_encoder_properties(&encoder_factory_name(encoding, &self.manual_config))
            {
                apply_property_value(&encoder, &property_name, &property_value);
            }
            for (property_name, property_value) in &self.manual_config.encoder_properties {
                apply_property_value(&encoder, property_name, property_value);
            }
        } else if self.encoding.is_some() {
            return Err(anyhow!(
                "Manual transcoding pipeline is missing the encoder element"
            ));
        }
        if let Some(decoder) = pipeline.by_name("decoder") {
            for (property_name, property_value) in &self.manual_config.decoder_properties {
                apply_property_value(&decoder, property_name, property_value);
            }
        }
        Ok(())
    }

    fn compressed_encoding(&self) -> Result<&'static dyn CompressedEncoding> {
        self.encoding
            .context("Manual transcoding pipeline is missing a compressed sink encoding")
    }
}

fn mjpg_encoder_input_capsfilter(
    encoding: &dyn CompressedEncoding,
) -> Result<Option<gst::Element>> {
    if encoding.encode_key() != "MJPG" {
        return Ok(None);
    }
    gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "I420")
                .build(),
        )
        .build()
        .map(Some)
        .context("Failed to create JPEG encoder input capsfilter")
}

fn add_compressed_source(
    pipeline: &gst::Pipeline,
    source_factory_name: &str,
    source_capsfilter: &gst::Element,
) -> Result<()> {
    let source = gst::ElementFactory::make(source_factory_name)
        .name("source")
        .build()
        .with_context(|| format!("Failed to create source {source_factory_name}"))?;
    if source.has_property("caps") {
        source.set_property("caps", source_capsfilter.property::<gst::Caps>("caps"));
    }
    pipeline
        .add(&source)
        .context("Failed to add source element")?;
    source
        .link(source_capsfilter)
        .context("Failed to link source to source capsfilter")?;
    Ok(())
}

fn encoder_factory_name(
    encoding: &dyn CompressedEncoding,
    manual_config: &ManualTranscodingConfig,
) -> String {
    if manual_config.encoder.is_empty() {
        encoding.preferred_encoder_factory().to_string()
    } else {
        manual_config.encoder.clone()
    }
}

fn decoder_factory_name(
    source_encode: &VideoEncodeType,
    manual_config: &ManualTranscodingConfig,
) -> Result<String> {
    if !manual_config.decoder.is_empty() {
        return Ok(manual_config.decoder.clone());
    }
    match source_encode {
        VideoEncodeType::Mjpg => Ok("jpegdec".to_string()),
        VideoEncodeType::H264 => Ok("avdec_h264".to_string()),
        VideoEncodeType::H265 => Ok("avdec_h265".to_string()),
        unsupported => Err(anyhow!(
            "Compressed format {unsupported:?} is not supported for manual transcoding"
        )),
    }
}

fn compressed_source_caps(
    source_encode: &VideoEncodeType,
    width: u32,
    height: u32,
    frame_interval: &FrameInterval,
) -> Result<gst::Caps> {
    let framerate = gst::Fraction::new(
        frame_interval.denominator as i32,
        frame_interval.numerator as i32,
    );
    match source_encode {
        VideoEncodeType::Mjpg => Ok(gst::Caps::builder("image/jpeg")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", framerate)
            .build()),
        VideoEncodeType::H264 => Ok(gst::Caps::builder("video/x-h264")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", framerate)
            .build()),
        VideoEncodeType::H265 => Ok(gst::Caps::builder("video/x-h265")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", framerate)
            .build()),
        unsupported => Err(anyhow!(
            "Compressed format {unsupported:?} is not supported for manual transcoding"
        )),
    }
}

pub fn startup_encoder_properties(
    factory_name: &str,
) -> std::collections::BTreeMap<String, PropertyValue> {
    let mut properties = std::collections::BTreeMap::new();
    match factory_name {
        "x264enc" => {
            properties.insert(
                "speed-preset".to_string(),
                PropertyValue::String("ultrafast".to_string()),
            );
            properties.insert(
                "tune".to_string(),
                PropertyValue::String("zerolatency".to_string()),
            );
            properties.insert("key-int-max".to_string(), PropertyValue::Integer(60));
            properties.insert("bitrate".to_string(), PropertyValue::Integer(4000));
            properties.insert("bframes".to_string(), PropertyValue::Integer(0));
            properties.insert(
                "threads".to_string(),
                PropertyValue::Integer(i64::from(x264enc_thread_count())),
            );
        }
        "jpegenc" => {
            properties.insert("quality".to_string(), PropertyValue::Integer(85));
            properties.insert("idct-method".to_string(), PropertyValue::Integer(1));
        }
        _ => {}
    }
    properties
}

fn x264enc_thread_count() -> u32 {
    std::thread::available_parallelism()
        .map(|count| (count.get() / 2).max(1) as u32)
        .unwrap_or(1)
}

fn raw_caps_format(source_encode: &VideoEncodeType) -> Result<&'static str> {
    match source_encode {
        VideoEncodeType::Nv12 => Ok("NV12"),
        VideoEncodeType::Yuyv => Ok("YUY2"),
        VideoEncodeType::Rgb => Ok("RGB"),
        unsupported => Err(anyhow!(
            "Raw format {unsupported:?} is not supported for manual transcoding"
        )),
    }
}

pub(crate) fn apply_property_value(element: &gst::Element, name: &str, value: &PropertyValue) {
    match value {
        PropertyValue::Bool(boolean) => try_set_property(element, name, boolean),
        PropertyValue::Integer(integer) => {
            if let Err(error) = set_property_from_api(element, name, *integer) {
                warn!("Failed to set encoder property {name} from integer {integer}: {error}");
            }
        }
        PropertyValue::Number(number) => try_set_property(element, name, number),
        PropertyValue::String(string) => try_set_property(element, name, string.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        stream::gst::encoding::{H264, H265, Mjpg},
        video::types::FrameInterval,
    };

    fn pipeline_has_factory(pipeline: &gst::Pipeline, factory_name: &str) -> bool {
        pipeline
            .iterate_recurse()
            .into_iter()
            .filter_map(Result::ok)
            .any(|element| {
                element
                    .factory()
                    .map(|factory| factory.name() == factory_name)
                    .unwrap_or(false)
            })
    }

    #[test]
    fn x264enc_startup_properties_set_tune_zerolatency() {
        let _ = gst::init();
        let encoder = gst::ElementFactory::make("x264enc")
            .build()
            .expect("x264enc");
        for (property_name, property_value) in startup_encoder_properties("x264enc") {
            apply_property_value(&encoder, &property_name, &property_value);
        }
        let value = encoder.property_value("tune");
        let (_class, flags) = gst::glib::FlagsValue::from_value(&value).expect("tune flags");
        assert!(
            flags.iter().any(|flag| flag.nick() == "zerolatency"),
            "tune={flags:?}"
        );
        let preset = encoder.property_value("speed-preset");
        let (_class, enum_value) =
            gst::glib::EnumValue::from_value(&preset).expect("speed-preset enum");
        assert_eq!(enum_value.nick(), "ultrafast");
    }

    #[test]
    fn h264_pipeline_includes_parser_and_payloader() {
        let _ = gst::init();
        if gst::ElementFactory::find("videotestsrc").is_none()
            || gst::ElementFactory::find("x264enc").is_none()
            || gst::ElementFactory::find("h264parse").is_none()
        {
            return;
        }
        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let transcoding_pipeline = ManualTranscodingPipeline {
            encoding: Some(&H264),
            source_encode: VideoEncodeType::Nv12,
            width: 640,
            height: 480,
            manual_config: ManualTranscodingConfig {
                encoder: "x264enc".to_string(),
                encoder_properties: Default::default(),
                decoder: String::new(),
                decoder_properties: Default::default(),
            },
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("camera", &pipeline_id, Some("videotestsrc"), None)
            .expect("build pipeline");

        assert!(pipeline.by_name("source").is_some());
        assert_eq!(
            pipeline
                .by_name("encoder")
                .and_then(|element| element.factory().map(|factory| factory.name().to_string())),
            Some("x264enc".to_string())
        );
        assert!(pipeline_has_factory(&pipeline, "h264parse"));
        assert!(pipeline_has_factory(&pipeline, "rtph264pay"));
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_RAW_TEE_NAME}-{pipeline_id}"))
                .is_some()
        );
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"))
                .is_some()
        );
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}"))
                .is_some()
        );
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}"))
                .is_some()
        );
    }

    #[test]
    fn mjpg_pipeline_skips_parser() {
        let _ = gst::init();
        if gst::ElementFactory::find("videotestsrc").is_none()
            || gst::ElementFactory::find("jpegenc").is_none()
        {
            return;
        }
        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let transcoding_pipeline = ManualTranscodingPipeline {
            encoding: Some(&Mjpg),
            source_encode: VideoEncodeType::Nv12,
            width: 640,
            height: 480,
            manual_config: ManualTranscodingConfig {
                encoder: "jpegenc".to_string(),
                encoder_properties: Default::default(),
                decoder: String::new(),
                decoder_properties: Default::default(),
            },
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("camera", &pipeline_id, Some("videotestsrc"), None)
            .expect("build pipeline");

        assert!(!pipeline_has_factory(&pipeline, "jpegparse"));
        assert!(pipeline_has_factory(&pipeline, "rtpjpegpay"));
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_RAW_TEE_NAME}-{pipeline_id}"))
                .is_some()
        );
        let filter = pipeline
            .by_name(&format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"))
            .expect("compressed capsfilter");
        let caps = filter.property::<gst::Caps>("caps");
        assert!(caps.to_string().contains("image/jpeg"));
    }

    #[test]
    fn h265_pipeline_includes_parser_and_payloader() {
        let _ = gst::init();
        if gst::ElementFactory::find("videotestsrc").is_none()
            || gst::ElementFactory::find("x265enc").is_none()
            || gst::ElementFactory::find("h265parse").is_none()
        {
            return;
        }
        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let transcoding_pipeline = ManualTranscodingPipeline {
            encoding: Some(&H265),
            source_encode: VideoEncodeType::Nv12,
            width: 640,
            height: 480,
            manual_config: ManualTranscodingConfig {
                encoder: "x265enc".to_string(),
                encoder_properties: Default::default(),
                decoder: String::new(),
                decoder_properties: Default::default(),
            },
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("camera", &pipeline_id, Some("videotestsrc"), None)
            .expect("build pipeline");

        assert_eq!(
            pipeline
                .by_name("encoder")
                .and_then(|element| element.factory().map(|factory| factory.name().to_string())),
            Some("x265enc".to_string())
        );
        assert!(pipeline_has_factory(&pipeline, "h265parse"));
        assert!(pipeline_has_factory(&pipeline, "rtph265pay"));
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_RAW_TEE_NAME}-{pipeline_id}"))
                .is_some()
        );
    }

    #[test]
    fn compressed_mjpg_pipeline_includes_decoder_not_raw_tee() {
        let _ = gst::init();
        if gst::ElementFactory::find("jpegdec").is_none() {
            return;
        }
        if gst::ElementFactory::find("x264enc").is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = ManualTranscodingPipeline {
            encoding: Some(&H264),
            source_encode: VideoEncodeType::Mjpg,
            width: 640,
            height: 480,
            manual_config: ManualTranscodingConfig {
                encoder: "x264enc".to_string(),
                encoder_properties: Default::default(),
                decoder: String::new(),
                decoder_properties: Default::default(),
            },
        };
        if gst::ElementFactory::find("appsrc").is_none() {
            return;
        }

        let pipeline = transcoding_pipeline
            .build_pipeline(
                "unused",
                &pipeline_id,
                Some("appsrc"),
                Some(&frame_interval),
            )
            .expect("build compressed manual pipeline");

        assert!(pipeline.by_name("source").is_some());
        assert_eq!(
            pipeline
                .by_name("decoder")
                .and_then(|element| element.factory().map(|factory| factory.name().to_string())),
            Some("jpegdec".to_string())
        );
        assert_eq!(
            pipeline
                .by_name("encoder")
                .and_then(|element| element.factory().map(|factory| factory.name().to_string())),
            Some("x264enc".to_string())
        );
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_RAW_TEE_NAME}-{pipeline_id}"))
                .is_none()
        );
        assert!(pipeline_has_factory(&pipeline, "h264parse"));
        assert!(pipeline_has_factory(&pipeline, "rtph264pay"));
    }

    #[test]
    fn compressed_to_raw_pipeline_uses_decoder_and_raw_payloader() {
        let _ = gst::init();
        if gst::ElementFactory::find("avdec_h264").is_none()
            || gst::ElementFactory::find("appsrc").is_none()
            || gst::ElementFactory::find("rtpvrawpay").is_none()
        {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = ManualTranscodingPipeline {
            encoding: None,
            source_encode: VideoEncodeType::H264,
            width: 640,
            height: 480,
            manual_config: ManualTranscodingConfig {
                encoder: String::new(),
                encoder_properties: Default::default(),
                decoder: String::new(),
                decoder_properties: Default::default(),
            },
        };
        let pipeline = transcoding_pipeline
            .build_pipeline(
                "unused",
                &pipeline_id,
                Some("appsrc"),
                Some(&frame_interval),
            )
            .expect("build manual decode pipeline");

        assert!(pipeline.by_name("source").is_some());
        assert_eq!(
            pipeline
                .by_name("decoder")
                .and_then(|element| element.factory().map(|factory| factory.name().to_string())),
            Some("avdec_h264".to_string())
        );
        assert!(pipeline.by_name("encoder").is_none());
        assert!(pipeline_has_factory(&pipeline, "rtpvrawpay"));
        assert!(pipeline_has_factory(&pipeline, "videoconvert"));
        let filter = pipeline
            .by_name(&format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"))
            .expect("raw capsfilter");
        let caps = filter.property::<gst::Caps>("caps");
        assert!(caps.to_string().contains("video/x-raw"));
        assert!(caps.to_string().contains("I420"));
        transcoding_pipeline
            .apply_runtime_properties(&pipeline)
            .expect("decode-only has no encoder to configure");
    }

    #[test]
    fn listed_encoders_can_encode() {
        let _ = gst::init();
        for encoding in crate::stream::gst::encoding::encodings() {
            let listed = crate::stream::gst::encoders::encoder_factory_names(*encoding);
            for name in &listed {
                crate::stream::gst::utils::encoder_factory_can_encode(*encoding, name)
                    .unwrap_or_else(|error| {
                        panic!(
                            "{name} was listed for {} but cannot encode: {error:#}",
                            encoding.encode_key()
                        )
                    });
            }

            let caps = gst::Caps::builder(encoding.caps_mime()).build();
            for factory in gst::ElementFactory::factories_with_type(
                gst::ElementFactoryType::VIDEO_ENCODER,
                gst::Rank::NONE,
            )
            .iter()
            {
                if !factory.can_src_any_caps(caps.as_ref()) {
                    continue;
                }
                let name = factory.name().to_string();
                if crate::stream::gst::utils::encoder_factory_can_encode(*encoding, &name).is_err()
                {
                    assert!(
                        !listed.iter().any(|listed_name| listed_name == &name),
                        "{name} failed dummy encode for {} but was still listed",
                        encoding.encode_key()
                    );
                }
            }
        }
    }
}
