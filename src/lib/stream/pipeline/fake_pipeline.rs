use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::{
        pipeline::{
            auto_transcoding::{AutoTranscodingPipeline, is_raw_encode},
            transcoding::{
                ManualTranscodingPipeline, apply_property_value, startup_encoder_properties,
            },
        },
        types::{CaptureConfiguration, SourceConfiguration},
    },
    video::{
        types::{VideoEncodeType, VideoSourceType},
        video_source_gst::VideoSourceGstType,
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME,
    PipelineGstreamerInterface, PipelineState,
};

#[derive(Debug)]
pub struct FakePipeline {
    pub state: PipelineState,
}

impl FakePipeline {
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        pipeline_id: &Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(configuration) => configuration,
            unsupported => {
                return Err(anyhow!("{unsupported:?} is not supported as Fake Pipeline"));
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "VideoSourceType {unsupported:?} is not supported as Fake Pipeline"
                ));
            }
        };

        let pattern = match &video_source.source {
            VideoSourceGstType::Fake(pattern) => pattern,
            unsupported => {
                return Err(anyhow!(
                    "VideoSourceGstType {unsupported:?} is not supported as Fake Pipeline"
                ));
            }
        };

        match &configuration.source_configuration {
            SourceConfiguration::Classic => {
                Self::try_new_classic(pipeline_id, configuration, pattern)
            }
            SourceConfiguration::AutoTranscoding(_) | SourceConfiguration::ManualTranscoding(_) => {
                Self::try_new_transcoding(pipeline_id, configuration, pattern)
            }
        }
    }

    fn try_new_transcoding(
        pipeline_id: &Arc<uuid::Uuid>,
        configuration: &crate::stream::types::VideoCaptureConfiguration,
        pattern: &str,
    ) -> Result<gst::Pipeline> {
        let raw_source = is_raw_encode(&configuration.source_encode);
        let source_factory_name = raw_source.then_some("videotestsrc");
        let pipeline = match &configuration.source_configuration {
            SourceConfiguration::AutoTranscoding(auto_config) => {
                let transcoding_pipeline = AutoTranscodingPipeline {
                    source_encode: configuration.source_encode.clone(),
                    sink_encode: configuration.sink_encode.clone(),
                    width: configuration.width,
                    height: configuration.height,
                    frame_interval: configuration.frame_interval,
                    auto_config: auto_config.clone(),
                };
                transcoding_pipeline
                    .build_pipeline("unused", pipeline_id, source_factory_name)
                    .context("Failed to build fake auto transcoding pipeline")?
            }
            SourceConfiguration::ManualTranscoding(manual_config) => {
                let encoding = crate::stream::gst::encoding::encoding(&configuration.sink_encode);
                if encoding.is_none() && !is_raw_encode(&configuration.sink_encode) {
                    return Err(anyhow!(
                        "Manual transcoding does not support sink_encode {:?}",
                        configuration.sink_encode
                    ));
                }
                let transcoding_pipeline = ManualTranscodingPipeline {
                    encoding,
                    source_encode: configuration.source_encode.clone(),
                    width: configuration.width,
                    height: configuration.height,
                    manual_config: manual_config.clone(),
                };
                let pipeline = transcoding_pipeline
                    .build_pipeline(
                        "unused",
                        pipeline_id,
                        source_factory_name,
                        (!raw_source).then_some(&configuration.frame_interval),
                    )
                    .context("Failed to build fake manual transcoding pipeline")?;
                transcoding_pipeline.apply_runtime_properties(&pipeline)?;
                pipeline
            }
            SourceConfiguration::Classic => {
                return Err(anyhow!(
                    "Classic fake pipelines are built by try_new_classic"
                ));
            }
        };

        if raw_source {
            configure_videotestsrc(&pipeline, pattern)?;
            insert_videoconvert_after_source(&pipeline)?;
        } else {
            attach_fake_compressed_source(&pipeline, &configuration.source_encode, pattern)?;
        }

        pipeline.set_property("name", format!("pipeline-fake-{pipeline_id}"));
        Ok(pipeline)
    }

    fn try_new_classic(
        pipeline_id: &Arc<uuid::Uuid>,
        configuration: &crate::stream::types::VideoCaptureConfiguration,
        pattern: &str,
    ) -> Result<gst::Pipeline> {
        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        // Fakes (videotestsrc) are only "video/x-raw" or "video/x-bayer",
        // and to be able to encode it, we need to define an available
        // format for both its src the next element's sink pad.
        // We are choosing "UYVY" because it is compatible with the
        // application-rtp template capabilities.
        // For more information: https://gstreamer.freedesktop.org/documentation/additional/design/mediatype-video-raw.html?gi-language=c#formats
        let description = match &configuration.source_encode {
            VideoEncodeType::H264 => {
                #[cfg(not(target_os = "windows"))]
                let format = "I420";

                #[cfg(target_os = "windows")]
                let format = "NV12";

                let capsfilter_profile = ",profile=constrained-baseline";

                #[cfg(target_os = "windows")]
                let h264_encoder = " ! mfh264enc low-latency=true bitrate=5000";

                #[cfg(not(target_os = "windows"))]
                let h264_encoder =
                    " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000";

                format!(
                    concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format={format}",
                        "{h264_encoder}",
                        " ! h264parse config-interval=-1",
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}{profile}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    h264_encoder = h264_encoder,
                    pattern = pattern,
                    format = format,
                    profile = capsfilter_profile,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::H265 => {
                #[cfg(not(target_os = "windows"))]
                let format = "I420";

                #[cfg(target_os = "windows")]
                let format = "NV12";

                #[cfg(target_os = "macos")]
                let h265_encoder = " ! vtenc_h265 allow-frame-reordering=false realtime=true quality=0.0 bitrate=5000";

                #[cfg(target_os = "windows")]
                let h265_encoder = " ! mfh265enc low-latency=true bitrate=5000";

                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let h265_encoder =
                    " ! x265enc tune=zerolatency speed-preset=ultrafast bitrate=5000";

                format!(
                    concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format={format}",
                        "{h265_encoder}",
                        " ! h265parse config-interval=-1",
                        " ! capsfilter name={filter_name} caps=video/x-h265,profile={profile},stream-format=byte-stream,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph265pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    h265_encoder = h265_encoder,
                    pattern = pattern,
                    format = format,
                    profile = "main",
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Yuyv => {
                format!(
                    concat!(
                        // Because application-rtp templates doesn't accept "YUY2", we
                        // need to transcode it. We are arbitrarily chosing the closest
                        // format available ("UYVY").
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format=I420",
                        " ! capsfilter name={filter_name} caps=video/x-raw,format=I420,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpvrawpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    pattern = pattern,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Mjpg => {
                format!(
                    concat!(
                        "videotestsrc pattern={pattern} is-live=true do-timestamp=true",
                        " ! timeoverlay",
                        " ! video/x-raw,format=I420",
                        " ! jpegenc quality=85 idct-method=1",
                        " ! capsfilter name={filter_name} caps=image/jpeg,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpjpegpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true",
                    ),
                    pattern = pattern,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for Test Pipeline"
                ));
            }
        };

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        pipeline.set_property("name", format!("pipeline-fake-{pipeline_id}"));

        Ok(pipeline)
    }
}

impl PipelineGstreamerInterface for FakePipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}

fn configure_videotestsrc(pipeline: &gst::Pipeline, pattern: &str) -> Result<()> {
    let source = pipeline
        .by_name("source")
        .context("Fake transcoding pipeline is missing the source element")?;
    if source.has_property("pattern") {
        source.set_property_from_str("pattern", pattern);
    }
    if source.has_property("is-live") {
        source.set_property("is-live", true);
    }
    if source.has_property("do-timestamp") {
        source.set_property("do-timestamp", true);
    }
    Ok(())
}

fn insert_videoconvert_after_source(pipeline: &gst::Pipeline) -> Result<()> {
    let source = pipeline
        .by_name("source")
        .context("Fake transcoding pipeline is missing the source element")?;
    let source_pad = source
        .static_pad("src")
        .context("Fake source element has no src pad")?;
    let peer_pad = source_pad
        .peer()
        .context("Fake source element is not linked")?;
    source_pad
        .unlink(&peer_pad)
        .context("Failed to unlink fake source from downstream")?;

    let videoconvert = gst::ElementFactory::make("videoconvert")
        .name("fake-source-videoconvert")
        .build()
        .context("Failed to create videoconvert for fake raw source")?;
    pipeline
        .add(&videoconvert)
        .context("Failed to add fake raw source videoconvert")?;
    source
        .link(&videoconvert)
        .context("Failed to link fake source to videoconvert")?;
    let convert_src = videoconvert
        .static_pad("src")
        .context("Fake source videoconvert has no src pad")?;
    convert_src
        .link(&peer_pad)
        .context("Failed to link fake source videoconvert to downstream")?;
    Ok(())
}

fn attach_fake_compressed_source(
    pipeline: &gst::Pipeline,
    source_encode: &VideoEncodeType,
    pattern: &str,
) -> Result<()> {
    let peer_pad = unlinked_static_sink_pad(pipeline, source_encode)?;

    let source = gst::ElementFactory::make("videotestsrc")
        .name("source")
        .build()
        .context("Failed to create videotestsrc for fake compressed source")?;
    if source.has_property("pattern") {
        source.set_property_from_str("pattern", pattern);
    }
    if source.has_property("is-live") {
        source.set_property("is-live", true);
    }
    if source.has_property("do-timestamp") {
        source.set_property("do-timestamp", true);
    }

    let videoconvert = gst::ElementFactory::make("videoconvert")
        .build()
        .context("Failed to create videoconvert for fake compressed source")?;
    let encoder_factory_name = fake_compressed_encoder_factory(source_encode)?;
    let encoder = gst::ElementFactory::make(encoder_factory_name)
        .name("fake-source-encoder")
        .build()
        .with_context(|| format!("Failed to create fake source encoder {encoder_factory_name}"))?;
    for (property_name, property_value) in startup_encoder_properties(encoder_factory_name) {
        apply_property_value(&encoder, &property_name, &property_value);
    }
    apply_classic_fake_encoder_properties(&encoder);

    let parser = compressed_source_parser(source_encode)?;

    pipeline
        .add(&source)
        .context("Failed to add fake videotestsrc")?;
    pipeline
        .add_many([&videoconvert, &encoder])
        .context("Failed to add fake compressed source encoder elements")?;
    if let Some(parser) = &parser {
        pipeline
            .add(parser)
            .context("Failed to add fake compressed source parser")?;
    }

    source
        .link(&videoconvert)
        .context("Failed to link fake source to videoconvert")?;
    videoconvert
        .link(&encoder)
        .context("Failed to link videoconvert to fake source encoder")?;
    let encoder_src = encoder
        .static_pad("src")
        .context("Fake source encoder has no src pad")?;
    if let Some(parser) = &parser {
        encoder
            .link(parser)
            .context("Failed to link fake source encoder to parser")?;
        let parser_src = parser
            .static_pad("src")
            .context("Fake source parser has no src pad")?;
        parser_src
            .link(&peer_pad)
            .context("Failed to link fake source parser to downstream")?;
    } else {
        encoder_src
            .link(&peer_pad)
            .context("Failed to link fake source encoder to downstream")?;
    }
    Ok(())
}

fn compressed_source_parser(source_encode: &VideoEncodeType) -> Result<Option<gst::Element>> {
    let parser_factory = match source_encode {
        VideoEncodeType::H264 => Some("h264parse"),
        VideoEncodeType::H265 => Some("h265parse"),
        _ => None,
    };
    let Some(parser_factory) = parser_factory else {
        return Ok(None);
    };
    let parser = gst::ElementFactory::make(parser_factory)
        .build()
        .with_context(|| format!("Failed to create fake source parser {parser_factory}"))?;
    if parser.has_property("config-interval") {
        parser.set_property("config-interval", -1i32);
    }
    Ok(Some(parser))
}

fn unlinked_static_sink_pad(
    pipeline: &gst::Pipeline,
    source_encode: &VideoEncodeType,
) -> Result<gst::Pad> {
    let expected_mime = match source_encode {
        VideoEncodeType::H264 => "video/x-h264",
        VideoEncodeType::H265 => "video/x-h265",
        VideoEncodeType::Mjpg => "image/jpeg",
        unsupported => {
            return Err(anyhow!(
                "Fake compressed source does not support {unsupported:?}"
            ));
        }
    };
    for element in pipeline.iterate_elements() {
        let element = element.map_err(|error| {
            anyhow!("Failed to iterate fake transcoding pipeline elements: {error}")
        })?;
        let Some(factory) = element.factory() else {
            continue;
        };
        if factory.name() != "capsfilter" {
            continue;
        }
        let caps = element.property::<gst::Caps>("caps");
        let Some(structure) = caps.structure(0) else {
            continue;
        };
        if structure.name().as_str() != expected_mime {
            continue;
        }
        let sink = element
            .static_pad("sink")
            .context("Compressed source capsfilter has no sink pad")?;
        if sink.is_linked() {
            continue;
        }
        return Ok(sink);
    }
    Err(anyhow!(
        "Fake compressed source has no unlinked {expected_mime} capsfilter"
    ))
}

fn fake_compressed_encoder_factory(source_encode: &VideoEncodeType) -> Result<&'static str> {
    let factory_name = match source_encode {
        VideoEncodeType::H264 => {
            #[cfg(target_os = "windows")]
            {
                "mfh264enc"
            }
            #[cfg(not(target_os = "windows"))]
            {
                "x264enc"
            }
        }
        VideoEncodeType::H265 => {
            #[cfg(target_os = "macos")]
            {
                "vtenc_h265"
            }
            #[cfg(target_os = "windows")]
            {
                "mfh265enc"
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                "x265enc"
            }
        }
        VideoEncodeType::Mjpg => "jpegenc",
        unsupported => {
            return Err(anyhow!(
                "Fake compressed source does not support {unsupported:?}"
            ));
        }
    };
    if gst::ElementFactory::find(factory_name).is_none() {
        return Err(anyhow!(
            "GStreamer encoder factory {factory_name} is not available"
        ));
    }
    Ok(factory_name)
}

fn apply_classic_fake_encoder_properties(encoder: &gst::Element) {
    if encoder.has_property("tune") {
        encoder.set_property_from_str("tune", "zerolatency");
    }
    if encoder.has_property("speed-preset") {
        encoder.set_property_from_str("speed-preset", "ultrafast");
    }
    if encoder.has_property("low-latency") {
        encoder.set_property("low-latency", true);
    }
    if encoder.has_property("bitrate") {
        encoder.set_property_from_str("bitrate", "5000");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        stream::{
            pipeline::auto_transcoding::{AUTO_DECODEBIN_NAME, AUTO_ENCODEBIN_NAME},
            types::{AutoTranscodingConfig, ManualTranscodingConfig, StreamInformation},
        },
        video::types::FrameInterval,
        video::video_source_gst::VideoSourceGst,
    };
    use url::Url;

    fn fake_video_and_stream(
        source_encode: VideoEncodeType,
        sink_encode: VideoEncodeType,
        source_configuration: SourceConfiguration,
    ) -> VideoAndStreamInformation {
        VideoAndStreamInformation {
            name: "fake-transcoding-test".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("udp://0.0.0.0:5600").unwrap()],
                configuration: CaptureConfiguration::Video(
                    crate::stream::types::VideoCaptureConfiguration {
                        source_encode,
                        sink_encode,
                        height: 240,
                        width: 320,
                        frame_interval: FrameInterval {
                            numerator: 1,
                            denominator: 30,
                        },
                        bit_depth: None,
                        source_configuration,
                        auto_restart_on_config_change: false,
                    },
                ),
                extended_configuration: None,
            },
            video_source: VideoSourceType::Gst(VideoSourceGst {
                name: "Fake".into(),
                source: VideoSourceGstType::Fake("ball".into()),
            }),
        }
    }

    fn play_until_rtp_buffer(
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
    ) -> Result<()> {
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");
        let rtp_tee = pipeline
            .by_name(&rtp_tee_name)
            .context("Fake transcoding pipeline is missing the RTP tee")?;
        let queue = gst::ElementFactory::make("queue")
            .build()
            .context("Failed to create probe queue")?;
        let fakesink = gst::ElementFactory::make("fakesink")
            .build()
            .context("Failed to create fakesink")?;
        fakesink.set_property("sync", false);
        fakesink.set_property("async", false);
        pipeline
            .add_many([&queue, &fakesink])
            .context("Failed to add RTP probe elements")?;
        queue
            .link(&fakesink)
            .context("Failed to link probe queue to fakesink")?;
        let tee_src = rtp_tee
            .request_pad_simple("src_%u")
            .context("Failed to request RTP tee src pad")?;
        let queue_sink = queue
            .static_pad("sink")
            .context("Probe queue has no sink pad")?;
        tee_src
            .link(&queue_sink)
            .context("Failed to link RTP tee to probe queue")?;

        let buffer_count = std::sync::atomic::AtomicU32::new(0);
        let buffer_count = std::sync::Arc::new(buffer_count);
        let probe_count = buffer_count.clone();
        queue_sink.add_probe(gst::PadProbeType::BUFFER, move |_pad, _info| {
            probe_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            gst::PadProbeReturn::Ok
        });

        pipeline
            .set_state(gst::State::Playing)
            .context("Failed to set fake transcoding pipeline to Playing")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let bus = pipeline
            .bus()
            .context("Fake transcoding pipeline has no bus")?;
        while std::time::Instant::now() < deadline {
            if buffer_count.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                let _ = pipeline.set_state(gst::State::Null);
                return Ok(());
            }
            if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
                if let gst::MessageView::Error(error) = message.view() {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Err(anyhow!(
                        "Fake transcoding pipeline bus error: {} ({})",
                        error.error(),
                        error.debug().unwrap_or_default()
                    ));
                }
            }
        }
        let _ = pipeline.set_state(gst::State::Null);
        Err(anyhow!("Fake transcoding pipeline produced no RTP buffers"))
    }

    #[test]
    fn auto_encode_uses_videotestsrc_and_encodebin() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_ENCODEBIN_NAME).is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let pipeline = FakePipeline::try_new(
            &pipeline_id,
            &fake_video_and_stream(
                VideoEncodeType::Nv12,
                VideoEncodeType::H264,
                SourceConfiguration::AutoTranscoding(AutoTranscodingConfig::default()),
            ),
        )
        .expect("build fake auto encode pipeline");

        let source = pipeline.by_name("source").expect("source element");
        assert_eq!(source.factory().unwrap().name(), "videotestsrc");
        assert!(pipeline.by_name(AUTO_ENCODEBIN_NAME).is_some());
        assert!(pipeline.by_name("fake-source-encoder").is_none());
        assert!(pipeline.by_name("fake-source-videoconvert").is_some());
        play_until_rtp_buffer(&pipeline, &pipeline_id)
            .unwrap_or_else(|error| panic!("auto encode should produce RTP buffers: {error}"));
    }

    #[test]
    fn manual_encode_uses_videotestsrc_and_named_encoder() {
        let _ = gst::init();
        if gst::ElementFactory::find("x264enc").is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let pipeline = FakePipeline::try_new(
            &pipeline_id,
            &fake_video_and_stream(
                VideoEncodeType::Nv12,
                VideoEncodeType::H264,
                SourceConfiguration::ManualTranscoding(ManualTranscodingConfig {
                    encoder: "x264enc".to_string(),
                    encoder_properties: Default::default(),
                    decoder: String::new(),
                    decoder_properties: Default::default(),
                }),
            ),
        )
        .expect("build fake manual encode pipeline");

        assert_eq!(
            pipeline
                .by_name("source")
                .expect("source element")
                .factory()
                .unwrap()
                .name(),
            "videotestsrc"
        );
        assert!(pipeline.by_name("encoder").is_some());
        assert!(pipeline.by_name("fake-source-encoder").is_none());
        play_until_rtp_buffer(&pipeline, &pipeline_id)
            .unwrap_or_else(|error| panic!("manual encode should produce RTP buffers: {error}"));
    }

    #[test]
    fn auto_decode_inserts_fake_compressed_source_encoder() {
        let _ = gst::init();
        if gst::ElementFactory::find(AUTO_DECODEBIN_NAME).is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let pipeline = FakePipeline::try_new(
            &pipeline_id,
            &fake_video_and_stream(
                VideoEncodeType::H264,
                VideoEncodeType::Nv12,
                SourceConfiguration::AutoTranscoding(AutoTranscodingConfig::default()),
            ),
        )
        .expect("build fake auto decode pipeline");

        assert!(pipeline.by_name("source").is_some());
        assert!(pipeline.by_name(AUTO_DECODEBIN_NAME).is_some());
        assert!(pipeline.by_name("fake-source-encoder").is_some());
    }

    #[test]
    fn manual_decode_inserts_fake_compressed_source_encoder() {
        let _ = gst::init();
        if gst::ElementFactory::find("avdec_h264").is_none() {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let pipeline = FakePipeline::try_new(
            &pipeline_id,
            &fake_video_and_stream(
                VideoEncodeType::H264,
                VideoEncodeType::Yuyv,
                SourceConfiguration::ManualTranscoding(ManualTranscodingConfig {
                    encoder: String::new(),
                    encoder_properties: Default::default(),
                    decoder: "avdec_h264".to_string(),
                    decoder_properties: Default::default(),
                }),
            ),
        )
        .expect("build fake manual decode pipeline");

        assert!(pipeline.by_name("source").is_some());
        assert!(pipeline.by_name("decoder").is_some());
        assert!(pipeline.by_name("fake-source-encoder").is_some());
    }

    #[test]
    fn manual_transcode_inserts_fake_compressed_source_encoder() {
        let _ = gst::init();
        if gst::ElementFactory::find("jpegdec").is_none()
            || gst::ElementFactory::find("x264enc").is_none()
        {
            return;
        }

        let pipeline_id = Arc::new(uuid::Uuid::nil());
        let pipeline = FakePipeline::try_new(
            &pipeline_id,
            &fake_video_and_stream(
                VideoEncodeType::Mjpg,
                VideoEncodeType::H264,
                SourceConfiguration::ManualTranscoding(ManualTranscodingConfig {
                    encoder: "x264enc".to_string(),
                    encoder_properties: Default::default(),
                    decoder: "jpegdec".to_string(),
                    decoder_properties: Default::default(),
                }),
            ),
        )
        .expect("build fake manual transcode pipeline");

        assert!(pipeline.by_name("source").is_some());
        assert!(pipeline.by_name("decoder").is_some());
        assert!(pipeline.by_name("encoder").is_some());
        assert!(pipeline.by_name("fake-source-encoder").is_some());
    }
}
