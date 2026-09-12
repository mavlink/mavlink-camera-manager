use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use glib::translate::FromGlibPtrFull;
use gst::prelude::*;
use tracing::warn;

use crate::{
    stream::{
        gst::encoding::CompressedEncoding,
        pipeline::transcoding::{apply_property_value, startup_encoder_properties},
        types::{AutoTranscodingConfig, SourceConfiguration, VideoCaptureConfiguration},
    },
    video::types::{FrameInterval, VideoEncodeType},
};

use super::{PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME};

pub const AUTO_ENCODEBIN_NAME: &str = "encodebin";
pub const AUTO_DECODEBIN_NAME: &str = "decodebin";
pub const AUTO_TRANSCODEBIN_NAME: &str = "transcodebin";
pub const AUTO_BIN_ELEMENT_NAMES: &[&str] = &[
    AUTO_ENCODEBIN_NAME,
    AUTO_DECODEBIN_NAME,
    AUTO_TRANSCODEBIN_NAME,
];

pub struct AutoTranscodingPipeline {
    pub source_encode: VideoEncodeType,
    pub sink_encode: VideoEncodeType,
    pub width: u32,
    pub height: u32,
    pub frame_interval: FrameInterval,
    pub auto_config: AutoTranscodingConfig,
}

#[derive(Clone, Copy)]
enum AutoTranscodeMode {
    EncodeOnly,
    DecodeOnly,
    Transcode,
}

impl AutoTranscodeMode {
    fn factory_name(self) -> &'static str {
        match self {
            AutoTranscodeMode::EncodeOnly => AUTO_ENCODEBIN_NAME,
            AutoTranscodeMode::DecodeOnly => AUTO_DECODEBIN_NAME,
            AutoTranscodeMode::Transcode => AUTO_TRANSCODEBIN_NAME,
        }
    }

    fn needs_encoding_profile(self) -> bool {
        !matches!(self, AutoTranscodeMode::DecodeOnly)
    }
}

impl AutoTranscodingPipeline {
    pub fn build_pipeline(
        &self,
        _device_path: &str,
        pipeline_id: &Arc<uuid::Uuid>,
        source_factory_name: Option<&str>,
    ) -> Result<gst::Pipeline> {
        let mode = auto_transcode_mode(&self.source_encode, &self.sink_encode)?;
        let bin_factory = mode.factory_name();
        if gst::ElementFactory::find(bin_factory).is_none() {
            return Err(anyhow!("GStreamer {bin_factory} is not available"));
        }

        let source_caps = source_caps(
            &self.source_encode,
            self.width,
            self.height,
            &self.frame_interval,
        )?;

        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let pipeline = gst::Pipeline::new();

        let source_capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", source_caps)
            .build()
            .context("Failed to create source capsfilter")?;

        let queue = gst::ElementFactory::make("queue")
            .build()
            .context("Failed to create queue")?;
        queue.set_property_from_str("leaky", "downstream");
        queue.set_property("max-size-buffers", 2u32);
        queue.set_property("max-size-time", gst::ClockTime::ZERO);
        queue.set_property("max-size-bytes", 0u32);

        let autobin = if mode.needs_encoding_profile() {
            let encoding = crate::stream::gst::encoding::encoding(&self.sink_encode)
                .with_context(|| format!("No compressed encoding for {:?}", self.sink_encode))?;
            let profile = sink_encoding_profile(encoding)?;
            gst::ElementFactory::make(bin_factory)
                .name(bin_factory)
                .property("profile", profile)
                .build()
                .with_context(|| format!("Failed to create {bin_factory}"))?
        } else {
            gst::ElementFactory::make(bin_factory)
                .name(bin_factory)
                .build()
                .with_context(|| format!("Failed to create {bin_factory}"))?
        };
        install_autobin_codec_property_hooks(&autobin, &self.auto_config, &self.sink_encode);

        let video_tee = gst::ElementFactory::make("tee")
            .name(video_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create video tee")?;

        let rtp_tee = gst::ElementFactory::make("tee")
            .name(rtp_tee_name.as_str())
            .property("allow-not-linked", true)
            .build()
            .context("Failed to create RTP tee")?;

        let mut tail_chain: Vec<gst::Element> = Vec::new();

        match mode {
            AutoTranscodeMode::DecodeOnly => {
                let videoconvert = gst::ElementFactory::make("videoconvert")
                    .build()
                    .context("Failed to create videoconvert")?;
                let raw_capsfilter = gst::ElementFactory::make("capsfilter")
                    .name(filter_name.as_str())
                    .property(
                        "caps",
                        delivery_raw_caps(self.width, self.height, &self.frame_interval)?,
                    )
                    .build()
                    .context("Failed to create raw capsfilter")?;
                let pay = gst::ElementFactory::make("rtpvrawpay")
                    .property("pt", 96u32)
                    .build()
                    .context("Failed to create rtpvrawpay")?;

                tail_chain.extend([
                    videoconvert,
                    raw_capsfilter,
                    video_tee.clone(),
                    pay,
                    rtp_tee.clone(),
                ]);
            }
            AutoTranscodeMode::EncodeOnly | AutoTranscodeMode::Transcode => {
                let encoding = crate::stream::gst::encoding::encoding(&self.sink_encode)
                    .with_context(|| {
                        format!("No compressed encoding for {:?}", self.sink_encode)
                    })?;
                // encodebin/transcodebin already include a parser; an external one
                // breaks H264 negotiation (encodebin outputs AVC, h264parse wants byte-stream).
                let compressed_capsfilter = gst::ElementFactory::make("capsfilter")
                    .name(filter_name.as_str())
                    .property("caps", encoding.compressed_caps(self.width, self.height))
                    .build()
                    .context("Failed to create compressed capsfilter")?;
                let pay = gst::ElementFactory::make(encoding.pay_factory_name())
                    .build()
                    .with_context(|| {
                        format!("Failed to create payloader {}", encoding.pay_factory_name())
                    })?;
                encoding.configure_pay_element(&pay);
                tail_chain.extend([
                    compressed_capsfilter,
                    video_tee.clone(),
                    pay,
                    rtp_tee.clone(),
                ]);
            }
        }

        let tail_head = tail_chain[0].clone();
        {
            let tail_head = tail_head.clone();
            autobin.connect_pad_added(move |_element, src_pad| {
                link_autobin_src_to_tail(src_pad, &tail_head, bin_factory);
            });
        }

        let tail_refs: Vec<&gst::Element> = tail_chain.iter().collect();
        pipeline
            .add_many(&tail_refs)
            .context("Failed to add auto transcoding tail elements")?;
        gst::Element::link_many(&tail_refs).context("Failed to link auto transcoding tail")?;

        pipeline
            .add_many([&source_capsfilter, &queue, &autobin])
            .context("Failed to add auto transcoding source chain elements")?;
        gst::Element::link_many([&source_capsfilter, &queue, &autobin])
            .context("Failed to link auto transcoding source chain")?;

        // encodebin's src pad is always present, so pad-added never fires for it.
        for src_pad in autobin.src_pads() {
            link_autobin_src_to_tail(&src_pad, &tail_head, bin_factory);
        }
        if matches!(mode, AutoTranscodeMode::EncodeOnly) && autobin.static_pad("src").is_some() {
            let sink_linked = tail_head
                .static_pad("sink")
                .is_some_and(|pad| pad.is_linked());
            if !sink_linked {
                return Err(anyhow!(
                    "encodebin src pad was not linked to the transcoding tail"
                ));
            }
        }

        if let Some(source_factory_name) = source_factory_name {
            let source = gst::ElementFactory::make(source_factory_name)
                .name("source")
                .build()
                .with_context(|| format!("Failed to create source {source_factory_name}"))?;
            pipeline
                .add(&source)
                .context("Failed to add source element")?;

            if let Some(parser_factory) = compressed_source_parser_factory(&self.source_encode) {
                let parser = gst::ElementFactory::make(parser_factory)
                    .build()
                    .with_context(|| format!("Failed to create source parser {parser_factory}"))?;
                pipeline
                    .add(&parser)
                    .context("Failed to add source parser element")?;
                gst::Element::link_many([&source, &parser, &source_capsfilter])
                    .context("Failed to link source through parser to source capsfilter")?;
            } else {
                source
                    .link(&source_capsfilter)
                    .context("Failed to link source to source capsfilter")?;
            }
        }

        Ok(pipeline)
    }
}

pub fn validate_video_capture_configuration(
    configuration: &VideoCaptureConfiguration,
) -> Result<()> {
    if configuration.source_encode == configuration.sink_encode {
        if matches!(
            configuration.source_configuration,
            SourceConfiguration::Classic
        ) {
            return Ok(());
        }
        return Err(anyhow!(
            "source_encode equals sink_encode but source_configuration is not Classic"
        ));
    }

    match &configuration.source_configuration {
        SourceConfiguration::Classic => Err(anyhow!(
            "source_encode {:?} differs from sink_encode {:?}; set source_configuration to auto or manual",
            configuration.source_encode,
            configuration.sink_encode
        )),
        SourceConfiguration::AutoTranscoding(_) => {
            auto_transcode_mode(&configuration.source_encode, &configuration.sink_encode)?;
            if is_compressed_encode(&configuration.sink_encode)
                && crate::stream::gst::encoding::encoding(&configuration.sink_encode).is_none()
            {
                return Err(anyhow!(
                    "Auto transcoding does not support sink_encode {:?}",
                    configuration.sink_encode
                ));
            }
            Ok(())
        }
        SourceConfiguration::ManualTranscoding(_) => {
            if is_raw_encode(&configuration.source_encode) {
                if !is_compressed_encode(&configuration.sink_encode) {
                    return Err(anyhow!(
                        "Manual raw transcoding only supports compressed sink_encode"
                    ));
                }
            } else if is_compressed_encode(&configuration.source_encode) {
                if !is_raw_encode(&configuration.sink_encode)
                    && !is_compressed_encode(&configuration.sink_encode)
                {
                    return Err(anyhow!(
                        "Manual compressed transcoding only supports compressed or raw sink_encode"
                    ));
                }
            } else {
                return Err(anyhow!(
                    "Manual transcoding does not support source_encode {:?}",
                    configuration.source_encode
                ));
            }
            if is_compressed_encode(&configuration.sink_encode)
                && crate::stream::gst::encoding::encoding(&configuration.sink_encode).is_none()
            {
                return Err(anyhow!(
                    "Manual transcoding does not support sink_encode {:?}",
                    configuration.sink_encode
                ));
            }
            Ok(())
        }
    }
}

pub fn is_raw_encode(encode: &VideoEncodeType) -> bool {
    matches!(
        encode,
        VideoEncodeType::Nv12 | VideoEncodeType::Yuyv | VideoEncodeType::Rgb
    )
}

pub fn is_compressed_encode(encode: &VideoEncodeType) -> bool {
    matches!(
        encode,
        VideoEncodeType::Mjpg | VideoEncodeType::H264 | VideoEncodeType::H265
    )
}

fn link_autobin_src_to_tail(src_pad: &gst::Pad, tail_head: &gst::Element, bin_factory: &str) {
    if src_pad.direction() != gst::PadDirection::Src {
        return;
    }
    let Some(sink_pad) = tail_head.static_pad("sink") else {
        return;
    };
    if sink_pad.is_linked() || src_pad.is_linked() {
        return;
    }
    if let Err(error) = src_pad.link(&sink_pad) {
        warn!("Failed to link {bin_factory} to downstream: {error}");
    }
}

fn compressed_source_parser_factory(source_encode: &VideoEncodeType) -> Option<&'static str> {
    match source_encode {
        VideoEncodeType::H264 => Some("h264parse"),
        VideoEncodeType::H265 => Some("h265parse"),
        _ => None,
    }
}

fn install_autobin_codec_property_hooks(
    autobin: &gst::Element,
    auto_config: &AutoTranscodingConfig,
    sink_encode: &VideoEncodeType,
) {
    let encoder_properties = auto_config.encoder_properties.clone();
    let decoder_properties = auto_config.decoder_properties.clone();
    let preferred_encoder_factory =
        crate::stream::gst::encoding::preferred_encoder_factory_name(sink_encode).to_string();
    let preferred_encoder_factory_for_signal = preferred_encoder_factory.clone();
    autobin.connect("element-added", false, move |values| {
        let element = values[1].get::<gst::Element>().ok()?;
        apply_autobin_codec_element_properties(
            &element,
            &decoder_properties,
            &encoder_properties,
            preferred_encoder_factory_for_signal.as_str(),
        );
        None
    });
    if let Ok(bin) = autobin.clone().downcast::<gst::Bin>() {
        for element in bin.iterate_recurse().into_iter().filter_map(Result::ok) {
            apply_autobin_codec_element_properties(
                &element,
                &auto_config.decoder_properties,
                &auto_config.encoder_properties,
                preferred_encoder_factory.as_str(),
            );
        }
    }
}

fn apply_autobin_codec_element_properties(
    element: &gst::Element,
    decoder_properties: &std::collections::BTreeMap<String, crate::stream::types::PropertyValue>,
    encoder_properties: &std::collections::BTreeMap<String, crate::stream::types::PropertyValue>,
    preferred_encoder_factory: &str,
) {
    if element
        .factory()
        .map(|factory| factory.klass().contains("Decoder"))
        .unwrap_or(false)
    {
        for (property_name, property_value) in decoder_properties {
            apply_property_value(element, property_name, property_value);
        }
        return;
    }
    if element
        .factory()
        .map(|factory| factory.klass().contains("Encoder"))
        .unwrap_or(false)
    {
        let factory_name = element
            .factory()
            .map(|factory| factory.name().to_string())
            .unwrap_or_else(|| preferred_encoder_factory.to_string());
        for (property_name, property_value) in startup_encoder_properties(&factory_name) {
            apply_property_value(element, &property_name, &property_value);
        }
        for (property_name, property_value) in encoder_properties {
            apply_property_value(element, property_name, property_value);
        }
    }
}

fn auto_transcode_mode(
    source_encode: &VideoEncodeType,
    sink_encode: &VideoEncodeType,
) -> Result<AutoTranscodeMode> {
    let raw_source = is_raw_encode(source_encode);
    let compressed_source = is_compressed_encode(source_encode);
    let raw_sink = is_raw_encode(sink_encode);
    let compressed_sink = is_compressed_encode(sink_encode);

    match (raw_source, compressed_source, raw_sink, compressed_sink) {
        (true, false, false, true) => Ok(AutoTranscodeMode::EncodeOnly),
        (false, true, true, false) => Ok(AutoTranscodeMode::DecodeOnly),
        (false, true, false, true) => Ok(AutoTranscodeMode::Transcode),
        _ => Err(anyhow!(
            "Auto transcoding does not support {source_encode:?} to {sink_encode:?}"
        )),
    }
}

fn source_caps(
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
        VideoEncodeType::Nv12 => Ok(gst::Caps::builder("video/x-raw")
            .field("format", "NV12")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", framerate)
            .build()),
        VideoEncodeType::Yuyv => Ok(gst::Caps::builder("video/x-raw")
            .field("format", "YUY2")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", framerate)
            .build()),
        VideoEncodeType::Rgb => Ok(gst::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .field("width", width as i32)
            .field("height", height as i32)
            .field("framerate", framerate)
            .build()),
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
            "Source format {unsupported:?} is not supported for auto transcoding"
        )),
    }
}

fn delivery_raw_caps(width: u32, height: u32, frame_interval: &FrameInterval) -> Result<gst::Caps> {
    let framerate = gst::Fraction::new(
        frame_interval.denominator as i32,
        frame_interval.numerator as i32,
    );
    Ok(gst::Caps::builder("video/x-raw")
        .field("format", "I420")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", framerate)
        .build())
}

fn sink_encoding_profile(encoding: &dyn CompressedEncoding) -> Result<glib::Object> {
    let format_caps = gst::Caps::builder(encoding.caps_mime()).build();
    unsafe {
        let profile = pbutils::gst_encoding_video_profile_new(
            format_caps.as_ptr() as *mut gst::ffi::GstCaps,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if profile.is_null() {
            return Err(anyhow!("Failed to create sink encoding profile"));
        }
        Ok(glib::Object::from_glib_full(
            profile as *mut glib::gobject_ffi::GObject,
        ))
    }
}

mod pbutils {
    use std::os::raw::{c_char, c_uint};

    #[link(name = "gstpbutils-1.0")]
    unsafe extern "C" {
        pub fn gst_encoding_video_profile_new(
            format: *mut gst::ffi::GstCaps,
            preset: *const c_char,
            presence_str: *const c_char,
            presence: c_uint,
        ) -> *mut glib::gobject_ffi::GObject;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{
        pipeline::PIPELINE_RAW_TEE_NAME,
        types::{ManualTranscodingConfig, VideoCaptureConfiguration},
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
    fn auto_transcode_mode_selects_encode_decode_or_transcode_bins() {
        assert!(matches!(
            auto_transcode_mode(&VideoEncodeType::Nv12, &VideoEncodeType::H264),
            Ok(AutoTranscodeMode::EncodeOnly)
        ));
        assert!(matches!(
            auto_transcode_mode(&VideoEncodeType::H264, &VideoEncodeType::Nv12),
            Ok(AutoTranscodeMode::DecodeOnly)
        ));
        assert!(matches!(
            auto_transcode_mode(&VideoEncodeType::Mjpg, &VideoEncodeType::H264),
            Ok(AutoTranscodeMode::Transcode)
        ));
    }

    #[test]
    fn nv12_to_h264_pipeline_uses_encodebin() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_ENCODEBIN_NAME).is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = AutoTranscodingPipeline {
            source_encode: VideoEncodeType::Nv12,
            sink_encode: VideoEncodeType::H264,
            width: 640,
            height: 480,
            frame_interval,
            auto_config: AutoTranscodingConfig::default(),
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("unused", &pipeline_id, Some("videotestsrc"))
            .expect("build NV12 to H264 auto pipeline");

        assert!(pipeline.by_name("source").is_some());
        let encodebin = pipeline
            .by_name(AUTO_ENCODEBIN_NAME)
            .expect("encodebin element");
        if let Some(encodebin_src) = encodebin.static_pad("src") {
            assert!(
                encodebin_src.is_linked(),
                "encodebin src pad must be linked at build time"
            );
        }
        assert!(pipeline.by_name(AUTO_TRANSCODEBIN_NAME).is_none());
        assert!(
            pipeline
                .by_name(&format!("{PIPELINE_RAW_TEE_NAME}-{pipeline_id}"))
                .is_none()
        );
        assert!(pipeline_has_factory(&pipeline, "rtph264pay"));
    }

    #[test]
    fn mjpg_to_h264_pipeline_uses_transcodebin() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_TRANSCODEBIN_NAME).is_none() {
            return;
        }
        if gst::ElementFactory::find("v4l2src").is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = AutoTranscodingPipeline {
            source_encode: VideoEncodeType::Mjpg,
            sink_encode: VideoEncodeType::H264,
            width: 640,
            height: 480,
            frame_interval,
            auto_config: AutoTranscodingConfig::default(),
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("/dev/video0", &pipeline_id, Some("v4l2src"))
            .expect("build MJPG to H264 auto pipeline");

        assert!(pipeline.by_name(AUTO_TRANSCODEBIN_NAME).is_some());
        assert!(pipeline.by_name(AUTO_ENCODEBIN_NAME).is_none());
        assert!(pipeline_has_factory(&pipeline, "rtph264pay"));
    }

    #[test]
    fn h264_to_nv12_pipeline_uses_decodebin() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_DECODEBIN_NAME).is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = AutoTranscodingPipeline {
            source_encode: VideoEncodeType::H264,
            sink_encode: VideoEncodeType::Nv12,
            width: 640,
            height: 480,
            frame_interval,
            auto_config: AutoTranscodingConfig::default(),
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("unused", &pipeline_id, None)
            .expect("build H264 to NV12 auto pipeline");

        assert!(pipeline.by_name("source").is_none());
        assert!(pipeline.by_name(AUTO_DECODEBIN_NAME).is_some());
        assert!(pipeline.by_name(AUTO_ENCODEBIN_NAME).is_none());
        assert!(pipeline_has_factory(&pipeline, "rtpvrawpay"));
        let filter = pipeline
            .by_name(&format!("{PIPELINE_FILTER_NAME}-{pipeline_id}"))
            .expect("raw capsfilter");
        let caps = filter.property::<gst::Caps>("caps");
        assert!(caps.to_string().contains("video/x-raw"));
        assert!(caps.to_string().contains("I420"));
    }

    #[test]
    fn nv12_to_h265_pipeline_uses_encodebin() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_ENCODEBIN_NAME).is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = AutoTranscodingPipeline {
            source_encode: VideoEncodeType::Nv12,
            sink_encode: VideoEncodeType::H265,
            width: 640,
            height: 480,
            frame_interval,
            auto_config: AutoTranscodingConfig::default(),
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("unused", &pipeline_id, Some("videotestsrc"))
            .expect("build NV12 to H265 auto pipeline");

        assert!(pipeline.by_name(AUTO_ENCODEBIN_NAME).is_some());
        assert!(pipeline_has_factory(&pipeline, "rtph265pay"));
    }

    #[test]
    fn mjpg_to_h265_pipeline_uses_transcodebin() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_TRANSCODEBIN_NAME).is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let frame_interval = FrameInterval {
            numerator: 1,
            denominator: 30,
        };
        let transcoding_pipeline = AutoTranscodingPipeline {
            source_encode: VideoEncodeType::Mjpg,
            sink_encode: VideoEncodeType::H265,
            width: 640,
            height: 480,
            frame_interval,
            auto_config: AutoTranscodingConfig::default(),
        };
        let pipeline = transcoding_pipeline
            .build_pipeline("unused", &pipeline_id, None)
            .expect("build MJPG to H265 auto pipeline");

        assert!(pipeline.by_name(AUTO_TRANSCODEBIN_NAME).is_some());
        assert!(pipeline_has_factory(&pipeline, "rtph265pay"));
    }

    #[test]
    fn validate_accepts_manual_compressed_to_raw() {
        let configuration = VideoCaptureConfiguration {
            source_encode: VideoEncodeType::H264,
            sink_encode: VideoEncodeType::Nv12,
            height: 480,
            width: 640,
            frame_interval: FrameInterval {
                numerator: 1,
                denominator: 30,
            },
            bit_depth: None,
            source_configuration: SourceConfiguration::ManualTranscoding(ManualTranscodingConfig {
                encoder: String::new(),
                encoder_properties: Default::default(),
                decoder: String::new(),
                decoder_properties: Default::default(),
            }),
            auto_restart_on_config_change: false,
        };
        validate_video_capture_configuration(&configuration).expect("H264 to NV12 manual decode");
    }

    #[test]
    fn validate_accepts_auto_h265_transitions() {
        let configuration = VideoCaptureConfiguration {
            source_encode: VideoEncodeType::Nv12,
            sink_encode: VideoEncodeType::H265,
            height: 480,
            width: 640,
            frame_interval: FrameInterval {
                numerator: 1,
                denominator: 30,
            },
            bit_depth: None,
            source_configuration: SourceConfiguration::AutoTranscoding(
                AutoTranscodingConfig::default(),
            ),
            auto_restart_on_config_change: false,
        };
        validate_video_capture_configuration(&configuration).expect("NV12 to H265 auto");
    }
}
