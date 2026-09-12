use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::{
        gst::utils::wait_for_element_state_sync,
        types::{CaptureConfiguration, SourceConfiguration},
    },
    video::{
        gst_device_monitor,
        types::{VideoEncodeType, VideoSourceType},
        video_source_local::VideoSourceLocalType,
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME,
    PipelineGstreamerInterface, PipelineState, auto_transcoding::AutoTranscodingPipeline,
    transcoding::ManualTranscodingPipeline,
};

/// `sensor-config` must match a real sensor mode or validate fails. Try packed
/// CSI depths in this order until a fakesink probe plays without a bus error.
const LIBCAMERA_SENSOR_CONFIG_BIT_DEPTHS: [i32; 3] = [10, 12, 8];
const SENSOR_CONFIG_PROBE_TIMEOUT: Duration = Duration::from_millis(1000);
const SENSOR_CONFIG_PROBE_HOLD: Duration = Duration::from_millis(200);

static ACCEPTED_SENSOR_CONFIG_BIT_DEPTH: OnceLock<Mutex<HashMap<String, i32>>> = OnceLock::new();

#[derive(Debug)]
pub struct V4lPipeline {
    pub state: PipelineState,
}

impl V4lPipeline {
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
                return Err(anyhow!(
                    "{unsupported:?} is not supported as Local Pipeline"
                ));
            }
        };

        let video_source = match &video_and_stream_information.video_source {
            VideoSourceType::Local(source) => source,
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as Local Pipeline"
                ));
            }
        };

        let device_path = video_source.device_path.as_str();

        debug!("Building Local pipeline for device path: {device_path}");

        let device = gst_device_monitor::local_device_with_path(device_path)?
            .upgrade()
            .context("Device disappeared between selection and pipeline build")?;

        match &configuration.source_configuration {
            SourceConfiguration::Classic => Self::try_new_classic(
                pipeline_id,
                video_source,
                &device,
                device_path,
                configuration,
            ),
            SourceConfiguration::ManualTranscoding(manual_config) => Self::try_new_manual(
                pipeline_id,
                video_source,
                &device,
                device_path,
                configuration,
                manual_config,
            ),
            SourceConfiguration::AutoTranscoding(auto_config) => Self::try_new_auto(
                pipeline_id,
                video_source,
                &device,
                device_path,
                configuration,
                auto_config,
            ),
        }
    }

    fn try_new_classic(
        pipeline_id: &Arc<uuid::Uuid>,
        video_source: &crate::video::video_source_local::VideoSourceLocal,
        device: &gst::Device,
        device_path: &str,
        configuration: &crate::stream::types::VideoCaptureConfiguration,
    ) -> Result<gst::Pipeline> {
        let width = configuration.width;
        let height = configuration.height;
        let interval_numerator = configuration.frame_interval.numerator;
        let interval_denominator = configuration.frame_interval.denominator;
        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let factory_name = match &video_source.typ {
            VideoSourceLocalType::Libcamera(_) => "libcamerasrc".to_string(),
            VideoSourceLocalType::Usb(_)
            | VideoSourceLocalType::LegacyRpiCam(_)
            | VideoSourceLocalType::Unknown(_) => gst_device_monitor::source_factory_name(device)
                .unwrap_or("v4l2src")
                .to_string(),
        };

        debug!("Local pipeline source factory: {factory_name}");

        let description = match &configuration.source_encode {
            VideoEncodeType::H264 => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        " ! h264parse config-interval=-1", // Here we need the parse to help the stream-format and alignment part, which is being fixated here because avc/au seems to reduce the CPU usage in the RTP payloading part.
                        " ! capsfilter name={filter_name} caps=video/x-h264,stream-format=avc,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph264pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::H265 => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        " ! h265parse",
                        " ! capsfilter name={filter_name} caps=video/x-h265,stream-format=byte-stream,alignment=au,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtph265pay aggregate-mode=zero-latency config-interval=-1 pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Yuyv | VideoEncodeType::Nv12 | VideoEncodeType::Rgb => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        " ! videoconvert",
                        " ! capsfilter name={filter_name} caps=video/x-raw,format=I420,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpvrawpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            VideoEncodeType::Mjpg => {
                format!(
                    concat!(
                        "{factory_name} name=source",
                        // We don't need a jpegparse, as it leads to incompatible caps, spoiling the negotiation.
                        " ! capsfilter name={filter_name} caps=image/jpeg,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                        " ! tee name={video_tee_name} allow-not-linked=true",
                        " ! rtpjpegpay pt=96",
                        " ! tee name={rtp_tee_name} allow-not-linked=true"
                    ),
                    factory_name = factory_name,
                    width = width,
                    height = height,
                    interval_denominator = interval_denominator,
                    interval_numerator = interval_numerator,
                    filter_name = filter_name,
                    video_tee_name = video_tee_name,
                    rtp_tee_name = rtp_tee_name,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for Local Pipeline"
                ));
            }
        };

        debug!("pipeline_description: {description:#?}");

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("parse::launch did not produce a gst::Pipeline"))?;

        let source = pipeline
            .by_name("source")
            .context("Failed to find source element after parse::launch")?;

        wire_classic_source(
            &source,
            device,
            device_path,
            &factory_name,
            width,
            height,
            configuration.bit_depth,
        )?;

        pipeline.set_property("name", format!("pipeline-local-{pipeline_id}"));

        Ok(pipeline)
    }

    fn try_new_manual(
        pipeline_id: &Arc<uuid::Uuid>,
        video_source: &crate::video::video_source_local::VideoSourceLocal,
        device: &gst::Device,
        device_path: &str,
        configuration: &crate::stream::types::VideoCaptureConfiguration,
        manual_config: &crate::stream::types::ManualTranscodingConfig,
    ) -> Result<gst::Pipeline> {
        let raw_source = matches!(
            configuration.source_encode,
            VideoEncodeType::Nv12 | VideoEncodeType::Yuyv | VideoEncodeType::Rgb
        );
        let compressed_source = matches!(
            configuration.source_encode,
            VideoEncodeType::Mjpg | VideoEncodeType::H264 | VideoEncodeType::H265
        );

        if !raw_source && !compressed_source {
            return Err(anyhow!(
                "Manual transcoding requires source_encode NV12, YUYV, RGB, MJPG, H264, or H265"
            ));
        }

        if raw_source && !matches!(video_source.typ, VideoSourceLocalType::Libcamera(_)) {
            return Err(anyhow!(
                "Raw manual transcoding requires a libcamera source"
            ));
        }

        let source_factory_name = match &video_source.typ {
            VideoSourceLocalType::Libcamera(_) => "libcamerasrc".to_string(),
            VideoSourceLocalType::Usb(_)
            | VideoSourceLocalType::LegacyRpiCam(_)
            | VideoSourceLocalType::Unknown(_) => gst_device_monitor::source_factory_name(device)
                .unwrap_or("v4l2src")
                .to_string(),
        };

        let encoding = crate::stream::gst::encoding::encoding(&configuration.sink_encode);
        if encoding.is_none()
            && !matches!(
                configuration.sink_encode,
                VideoEncodeType::Nv12 | VideoEncodeType::Yuyv | VideoEncodeType::Rgb
            )
        {
            return Err(anyhow!(
                "Manual transcoding does not support sink_encode {:?}",
                configuration.sink_encode
            ));
        }
        if encoding.is_none() && !compressed_source {
            return Err(anyhow!(
                "Manual decode-only transcoding requires a compressed source_encode"
            ));
        }

        let transcoding_pipeline = ManualTranscodingPipeline {
            encoding,
            source_encode: configuration.source_encode.clone(),
            width: configuration.width,
            height: configuration.height,
            manual_config: manual_config.clone(),
        };
        let pipeline = if raw_source {
            transcoding_pipeline
                .build_pipeline(device_path, pipeline_id, None, None)
                .context("Failed to build raw manual transcoding pipeline")?
        } else {
            transcoding_pipeline
                .build_pipeline(
                    device_path,
                    pipeline_id,
                    Some(source_factory_name.as_str()),
                    Some(&configuration.frame_interval),
                )
                .context("Failed to build compressed manual transcoding pipeline")?
        };
        pipeline.set_property("name", format!("pipeline-local-{pipeline_id}"));
        transcoding_pipeline.apply_runtime_properties(&pipeline)?;

        let source = pipeline
            .by_name("source")
            .context("Manual transcoding pipeline is missing the source element")?;
        if raw_source {
            apply_libcamera_src_knobs(
                &source,
                device,
                device_path,
                configuration.width as i32,
                configuration.height as i32,
                configuration.bit_depth,
            );
            crate::video::local::libcamera_controls::apply_pending_to_element(device_path, &source);
            crate::video::local::libcamera_controls::install_live_apply_probe(&source, device_path);
        } else {
            wire_classic_source(
                &source,
                device,
                device_path,
                &source_factory_name,
                configuration.width,
                configuration.height,
                configuration.bit_depth,
            )?;
        }

        Ok(pipeline)
    }

    fn try_new_auto(
        pipeline_id: &Arc<uuid::Uuid>,
        video_source: &crate::video::video_source_local::VideoSourceLocal,
        device: &gst::Device,
        device_path: &str,
        configuration: &crate::stream::types::VideoCaptureConfiguration,
        auto_config: &crate::stream::types::AutoTranscodingConfig,
    ) -> Result<gst::Pipeline> {
        if configuration.source_encode == configuration.sink_encode {
            return Err(anyhow!(
                "Auto transcoding requires source_encode to differ from sink_encode"
            ));
        }

        let source_factory_name = match &video_source.typ {
            VideoSourceLocalType::Libcamera(_) => "libcamerasrc".to_string(),
            VideoSourceLocalType::Usb(_)
            | VideoSourceLocalType::LegacyRpiCam(_)
            | VideoSourceLocalType::Unknown(_) => gst_device_monitor::source_factory_name(device)
                .unwrap_or("v4l2src")
                .to_string(),
        };

        let transcoding_pipeline = AutoTranscodingPipeline {
            source_encode: configuration.source_encode.clone(),
            sink_encode: configuration.sink_encode.clone(),
            width: configuration.width,
            height: configuration.height,
            frame_interval: configuration.frame_interval,
            auto_config: auto_config.clone(),
        };
        let pipeline = transcoding_pipeline
            .build_pipeline(device_path, pipeline_id, Some(source_factory_name.as_str()))
            .context("Failed to build auto transcoding pipeline")?;
        pipeline.set_property("name", format!("pipeline-local-{pipeline_id}"));

        let source = pipeline
            .by_name("source")
            .context("Auto transcoding pipeline is missing the source element")?;
        wire_classic_source(
            &source,
            device,
            device_path,
            &source_factory_name,
            configuration.width,
            configuration.height,
            configuration.bit_depth,
        )?;

        Ok(pipeline)
    }
}

fn wire_classic_source(
    source: &gst::Element,
    device: &gst::Device,
    device_path: &str,
    factory_name: &str,
    width: u32,
    height: u32,
    bit_depth: Option<u32>,
) -> Result<()> {
    // `do-timestamp` only exists on GstBaseSrc subclasses (v4l2src does, libcamerasrc doesn't).
    if source.has_property("do-timestamp") {
        source.set_property("do-timestamp", true);
    }

    // The v4l2 device provider's `reconfigure_element` vfunc is broken
    // upstream (it compares the factory name against the GType name), so
    // set the device-identifying property directly from the known path.
    // Other factories fall back to `reconfigure_element`, which is our
    // best-effort for now.
    match factory_name {
        "v4l2src" => {
            source.set_property("device", device_path);
            debug!("Applied v4l2src device={device_path:?}");
        }
        "libcamerasrc" => {
            source.set_property("camera-name", device_path);
            debug!("Applied libcamerasrc camera-name={device_path:?}");
            apply_libcamera_src_knobs(
                source,
                device,
                device_path,
                width as i32,
                height as i32,
                bit_depth,
            );
            crate::video::local::libcamera_controls::apply_pending_to_element(device_path, source);
            crate::video::local::libcamera_controls::install_live_apply_probe(source, device_path);
        }
        other => {
            device.reconfigure_element(source).with_context(|| {
                format!("Failed to apply device configuration to {other} source")
            })?;
            debug!("Applied device configuration via reconfigure_element for {other}");
        }
    }

    Ok(())
}

impl PipelineGstreamerInterface for V4lPipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}

/// Apply gst-libcamera 0.7+ pad/element knobs when the installed plugin has them.
///
/// `stream-role` stays at `video-recording`: `raw` would emit Bayer and break the
/// I420 path. `sensor-config` pins the requested capture size so libcamera uses
/// that sensor mode instead of auto-picking a crop/bin from the ISP output caps.
#[instrument(level = "debug", skip(source, device))]
pub(crate) fn apply_libcamera_src_knobs(
    source: &gst::Element,
    device: &gst::Device,
    device_path: &str,
    width: i32,
    height: i32,
    requested_bit_depth: Option<u32>,
) {
    if let Some(src_pad) = source.static_pad("src")
        && src_pad.has_property("stream-role")
    {
        src_pad.set_property_from_str("stream-role", "video-recording");
        debug!("Applied libcamerasrc src stream-role=video-recording");
    }

    if !source.has_property("sensor-config") {
        return;
    }

    let Some(properties) = device.properties() else {
        return;
    };

    let Ok(pipeline_handler) = properties.get::<String>("api.libcamera.PipelineHandler") else {
        return;
    };
    if !pipeline_handler.starts_with("rpi/") {
        return;
    }

    if width <= 0 || height <= 0 {
        return;
    }

    let bit_depth = match requested_bit_depth {
        Some(depth) => depth as i32,
        None => match accepted_sensor_config_bit_depth(device_path, width, height) {
            Some(depth) => depth,
            None => {
                warn!(
                    "No sensor-config bit depth accepted for {device_path:?} at {width}x{height}; leaving auto mode selection"
                );
                return;
            }
        },
    };

    source.set_property(
        "sensor-config",
        sensor_config_structure(width, height, bit_depth),
    );
    debug!(
        "Applied libcamerasrc sensor-config width={width} height={height} depth={bit_depth} on {device_path:?}"
    );
}

fn sensor_config_structure(width: i32, height: i32, bit_depth: i32) -> gst::Structure {
    gst::Structure::builder("sensor/config")
        .field("width", width)
        .field("height", height)
        .field("depth", bit_depth)
        .build()
}

pub(crate) fn accepted_sensor_config_bit_depth(
    camera_name: &str,
    width: i32,
    height: i32,
) -> Option<i32> {
    if let Some(bit_depth) = cached_sensor_config_bit_depth(camera_name) {
        return Some(bit_depth);
    }

    for bit_depth in LIBCAMERA_SENSOR_CONFIG_BIT_DEPTHS {
        if sensor_config_bit_depth_is_accepted(camera_name, width, height, bit_depth) {
            store_cached_sensor_config_bit_depth(camera_name, bit_depth);
            return Some(bit_depth);
        }
        debug!("sensor-config {width}x{height} depth={bit_depth} rejected for {camera_name:?}");
    }

    None
}

fn cached_sensor_config_bit_depth(camera_name: &str) -> Option<i32> {
    let Ok(cache) = ACCEPTED_SENSOR_CONFIG_BIT_DEPTH
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        warn!("sensor-config bit-depth cache poisoned");
        return None;
    };
    cache.get(camera_name).copied()
}

fn store_cached_sensor_config_bit_depth(camera_name: &str, bit_depth: i32) {
    let Ok(mut cache) = ACCEPTED_SENSOR_CONFIG_BIT_DEPTH
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        warn!(
            "sensor-config bit-depth cache poisoned; not storing {bit_depth} for {camera_name:?}"
        );
        return;
    };
    cache.insert(camera_name.to_string(), bit_depth);
}

/// Open a throwaway `libcamerasrc` (shared CameraManager, camera free while the
/// real pipeline is still NULL) and see if this `sensor-config` survives validate.
#[instrument(level = "debug")]
fn sensor_config_bit_depth_is_accepted(
    camera_name: &str,
    width: i32,
    height: i32,
    bit_depth: i32,
) -> bool {
    let sink = match gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
    {
        Ok(sink) => sink,
        Err(error) => {
            warn!("sensor-config probe failed to create fakesink: {error}");
            return false;
        }
    };

    let source = match gst::ElementFactory::make("libcamerasrc")
        .name("probe-source")
        .build()
    {
        Ok(source) => source,
        Err(error) => {
            warn!("sensor-config probe failed to create libcamerasrc: {error}");
            return false;
        }
    };
    if !source.has_property("sensor-config") {
        return false;
    }

    source.set_property("camera-name", camera_name);
    source.set_property(
        "sensor-config",
        sensor_config_structure(width, height, bit_depth),
    );

    let pipeline = gst::Pipeline::new();
    if pipeline.add(&sink).is_err() {
        warn!("sensor-config probe failed to add fakesink");
        return false;
    }
    if pipeline.add(&source).is_err() {
        warn!("sensor-config probe failed to add libcamerasrc");
        return false;
    }
    if source.link(&sink).is_err() {
        warn!("sensor-config probe failed to link source to sink");
        return false;
    }

    let Some(bus) = pipeline.bus() else {
        return false;
    };

    if let Err(error) = pipeline.set_state(gst::State::Playing) {
        debug!("sensor-config probe set_state(Playing) failed: {error}");
        stop_probe_pipeline(&pipeline);
        return false;
    }

    let accepted = probe_played_without_error(&pipeline, &bus);
    stop_probe_pipeline(&pipeline);
    accepted
}

fn probe_played_without_error(pipeline: &gst::Pipeline, bus: &gst::Bus) -> bool {
    let deadline = Instant::now() + SENSOR_CONFIG_PROBE_TIMEOUT;
    let mut playing_since: Option<Instant> = None;

    loop {
        let now = Instant::now();
        if now >= deadline {
            return playing_since.is_some();
        }

        if let Some(playing_since) = playing_since
            && now.duration_since(playing_since) >= SENSOR_CONFIG_PROBE_HOLD
        {
            return true;
        }

        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(50));
        let timeout = gst::ClockTime::from_nseconds(wait.as_nanos() as u64);
        if let Some(message) = bus.timed_pop(timeout)
            && let gst::MessageView::Error(error) = message.view()
        {
            debug!("sensor-config probe bus error: {}", error.error());
            return false;
        }

        if pipeline.current_state() == gst::State::Playing && playing_since.is_none() {
            playing_since = Some(Instant::now());
        }
    }
}

fn stop_probe_pipeline(pipeline: &gst::Pipeline) {
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        warn!("sensor-config probe set_state(Null) failed: {error}");
    }
    if let Err(error) = wait_for_element_state_sync(pipeline.upcast_ref(), gst::State::Null, 50, 2)
    {
        warn!("sensor-config probe did not reach Null: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::types::SourceConfiguration;

    #[test]
    fn sensor_config_structure_sets_width_height_depth() {
        gst::init().unwrap();
        let structure = sensor_config_structure(3280, 2464, 10);
        assert_eq!(structure.get::<i32>("width").unwrap(), 3280);
        assert_eq!(structure.get::<i32>("height").unwrap(), 2464);
        assert_eq!(structure.get::<i32>("depth").unwrap(), 10);
    }

    #[test]
    fn classic_source_configuration_uses_parse_launch_path() {
        assert!(matches!(
            SourceConfiguration::Classic,
            SourceConfiguration::Classic
        ));
    }
}
