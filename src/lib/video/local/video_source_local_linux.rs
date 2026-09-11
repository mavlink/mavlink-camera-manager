use std::{
    cmp::max,
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use gst::prelude::*;
use paperclip::actix::Apiv2Schema;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::*;

use crate::{
    controls::types::*,
    stream::manager::{LiveSourceLookup, try_any_live_libcamerasrc},
    stream::types::VideoCaptureConfiguration,
    video::{
        gst_device_monitor,
        types::*,
        video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats},
    },
};

/// When `1`/`true`/`yes`/`on`, libcamera `/v4l` listings keep only native
/// sensor-mode fps. Common rates (60, 30, 24, 16, 10, 5) at or below that max
/// are omitted.
const LIBCAMERA_NATIVE_FPS_ONLY_ENV: &str = "MCM_LIBCAMERA_NATIVE_FPS_ONLY";
const LIBCAMERA_FORMAT_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

static LIBCAMERA_NATIVE_SIZES: OnceLock<Mutex<HashMap<String, Vec<Size>>>> = OnceLock::new();
static DEVICE_FORMATS: OnceLock<Mutex<HashMap<String, Vec<Format>>>> = OnceLock::new();
static DEVICE_CONTROLS: OnceLock<Mutex<HashMap<String, Vec<Control>>>> = OnceLock::new();

/// Helper function to wrap calls from v4l that can cause panic, returning an error instead
fn unpanic<T, F>(body: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::Builder::new()
        .name("v4l_wrap".to_string())
        .spawn(body)
        .expect("Failed to spawn thread")
        .join()
        .inspect_err(|e| error!("v4l API failed with: {:?}", e.downcast_ref::<String>()))
        .unwrap()
}

//TODO: Move to types
#[derive(Apiv2Schema, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum VideoSourceLocalType {
    Unknown(String),
    Usb(String),
    LegacyRpiCam(String),
    Libcamera(String),
}

#[derive(Apiv2Schema, Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct VideoSourceLocal {
    pub name: String,
    pub device_path: String,
    #[serde(rename = "type")]
    pub typ: VideoSourceLocalType,
}

impl VideoSourceLocalType {
    // For PCI:
    // https://wiki.xenproject.org/wiki/Bus:Device.Function_(BDF)_Notation
    // description should follow: <domain>:<bus>:<device>.<first_function>-<last_function>
    // E.g: usb-0000:08:00.3-1, where domain, bus and device are hexadecimal
    // `first_function` describes the usb HUB and `last_function` describes the USB port of that HUB
    //
    // For devices that does not have PCI, the information will come with
    // the following description: usb-<unknown>z.<usb-usb_hub>.<usb_port>
    // E.g: usb-3f980000.usb-1.4, where unknown is hexadecimal
    // `udevadm info` can also provide information about the camera
    //
    // https://www.kernel.org/doc/html/v4.9/media/uapi/v4l/vidioc-querycap.html#:~:text=__u8-,bus_info,-%5B32%5D

    #[instrument(level = "debug")]
    pub fn from_str(description: &str) -> Self {
        if let Some(result) = VideoSourceLocalType::usb_from_str(description) {
            return result;
        }

        if let Some(result) = VideoSourceLocalType::v4l2_from_str(description) {
            return result;
        }

        let msg = format!(
            "Unable to identify the local camera connection type, please report the problem: {description:?}"
        );
        if description == "platform:bcm2835-isp" {
            // Filter out the log for this particular device, because regarding to Raspberry Pis, it will always be there and we will never use it.
            trace!(msg);
        } else {
            warn!(msg);
        }
        VideoSourceLocalType::Unknown(description.into())
    }

    #[instrument(level = "debug")]
    fn usb_from_str(description: &str) -> Option<Self> {
        let regex = match Regex::new(
            r"usb-(?P<interface>(([0-9a-fA-F]{2}){1,2}:?){4})?\.(usb-)?(?P<device>.*)",
        ) {
            Ok(regex) => regex,
            Err(error) => {
                error!("Failed to construct regex: {error:?}");
                return None;
            }
        };

        if regex.is_match(description) {
            return Some(VideoSourceLocalType::Usb(description.into()));
        }
        None
    }

    #[instrument(level = "debug")]
    fn v4l2_from_str(description: &str) -> Option<Self> {
        let regex = match Regex::new(r"platform:(?P<device>\S+)-v4l2-[0-9]") {
            Ok(regex) => regex,
            Err(error) => {
                error!("Failed to construct regex: {error:?}");
                return None;
            }
        };

        if regex.is_match(description) {
            return Some(VideoSourceLocalType::LegacyRpiCam(description.into()));
        }
        None
    }
}

impl VideoSourceLocal {
    #[instrument(level = "debug", skip(formats))]
    pub async fn try_identify_device(
        &mut self,
        capture_configuration: &VideoCaptureConfiguration,
        candidates: &[VideoSourceType],
        formats: &HashMap<String, Vec<Format>>,
    ) -> Result<Option<String>> {
        // Rule n.1 - All candidates must share the same camera name
        let candidates = Self::get_cameras_with_same_name(candidates, &self.name);

        let len = candidates.len();
        if len == 0 {
            // Outcome n.1 - This happens when all cameras connected has changed since last settings
            trace!("Outcome n.1!");
            return Err(anyhow!("Device not found"));
        }

        // Rule n.2 - All candidates must share the same encode
        let candidates = Self::get_cameras_with_same_encode(
            &candidates,
            &capture_configuration.source_encode,
            formats,
        );

        let len = candidates.len();
        if len == 0 {
            // Outcome n.2 - This could only happen if there are cameras (not devices) with the same name
            // but different encodes, maybe if the kernel fail to add (or remove) one of the camera's linux
            // devices, for example, for a camera that originally has two devices, say `/dev/video0` (with
            // only H264 encode), and `/dev/video1` (with YUYV and MJPG encodes), and for any reason,
            // suddently `/dev/video1` is unmounted, then we'd be invalidating `/dev/video1` while keeping
            // `/dev/video0`.
            trace!("Outcome n.2!");
            return Err(anyhow!("Device not found"));
        }
        if len == 1 {
            // Outcome n.3 - This happens when we change cameras from one USB port to another
            // This can happen ocasionally, by chance, when the kernel enumerates the devices
            // in a different order making, say, `/dev/video0` (initially with H264 encode)
            // appear interchangibly as `/dev/video1`, which initially had YUYV and MJPG
            // encodes. This can happens everytime we reconnect a USB, or restart the OS.
            trace!("Outcome n.3!");
            let candidate = &candidates[0];
            return Ok(Some(candidate.inner().source_string().to_string()));
        }

        // Rule n.3 - Same name, same encode, same USB port.
        let candidates = Self::get_cameras_with_same_bus(&candidates, &self.typ);

        let len = candidates.len();
        if len == 1 {
            trace!("Outcome n.4!");
            let candidate = &candidates[0];
            return Ok(Some(candidate.inner().source_string().to_string()));
        }

        // Outcome n.5 - If there are several candidates left, and as we lack other methods to diferentiate
        // them, it's better to not change anything, keeping their identity as it is. This is the case when
        // we have two or more identical USB cameras connected, let's say we have, initially, two cameras
        // connected:
        // - USB port A with camera `Alpha`, receiving devices /dev/video0 (H264) and /dev/video1 (MJPG, YUYV)
        // - USB port B with camera `Beta`, receiving devices /dev/video2 (H264) and /dev/video3 (MJPG, YUYV).
        // Then in the next reboot, `Alpha` changed to port B, and `Beta` was changed to port A, then it's
        // impossible to differentiate them using only the name.
        trace!("Outcome n.5!");
        warn!(
            "There is more than one camera with the same name and encode, which means that their identification/configurations could have been swaped"
        );
        Ok(None)
    }

    #[instrument(level = "debug")]
    fn get_cameras_with_same_name(
        candidates: &[VideoSourceType],
        name: &str,
    ) -> Vec<VideoSourceType> {
        candidates
            .iter()
            .filter(|candidate| {
                let VideoSourceType::Local(camera) = candidate else {
                    return false;
                };

                camera.name == name
            })
            .cloned()
            .collect()
    }

    #[instrument(level = "debug", skip(formats))]
    fn get_cameras_with_same_encode(
        candidates: &[VideoSourceType],
        encode: &VideoEncodeType,
        formats: &HashMap<String, Vec<Format>>,
    ) -> Vec<VideoSourceType> {
        candidates
            .iter()
            .filter(|candidate| {
                formats
                    .get(candidate.inner().source_string())
                    .is_some_and(|fs| fs.iter().any(|format| &format.encode == encode))
            })
            .cloned()
            .collect()
    }

    #[instrument(level = "debug")]
    fn get_cameras_with_same_bus(
        candidates: &[VideoSourceType],
        typ: &VideoSourceLocalType,
    ) -> Vec<VideoSourceType> {
        candidates
            .iter()
            .filter(|candidate| {
                let VideoSourceType::Local(camera) = candidate else {
                    return false;
                };
                &camera.typ == typ
            })
            .cloned()
            .collect()
    }
}

fn compute_intervals_from_range(
    interval_start: FrameInterval,
    interval_end: FrameInterval,
    step: usize,
) -> Vec<FrameInterval> {
    let mut intervals = Vec::with_capacity(20);

    // To avoid having a huge number of numerator/denominators, we
    // arbitrarily set a minimum step of 5 units
    let step = step.max(5);

    let numerator_end = interval_end.numerator.min(30);
    let denominator_end = interval_end.denominator.min(30);

    let min_numerator = max(1, interval_start.numerator);
    let min_denominator = max(1, interval_start.denominator);

    for numerator in (0..=numerator_end).step_by(step) {
        for denominator in (0..=denominator_end).step_by(step) {
            intervals.push(FrameInterval {
                numerator: numerator.max(min_numerator),
                denominator: denominator.max(min_denominator),
            });
        }
    }

    intervals
}

impl From<gst::Fraction> for FrameInterval {
    fn from(value: gst::Fraction) -> Self {
        FrameInterval {
            // Yes, our nominator is GST's denominator.
            numerator: value.denom() as u32,
            denominator: value.numer() as u32,
        }
    }
}

#[cfg(test)]
fn libcamera_pixel_array_size(properties: &gst::StructureRef) -> Option<(i32, i32)> {
    let array = properties
        .get::<gst::Array>("api.libcamera.PixelArraySize")
        .ok()?;
    let values = array.as_slice();
    if values.len() != 2 {
        return None;
    }
    Some((values[0].get::<i32>().ok()?, values[1].get::<i32>().ok()?))
}

/// Highest advertised fps for a discrete `width`×`height` in `caps`.
/// Ignores `GstIntRange` ISP scaler entries and does not invent default rates.
fn libcamera_max_frame_interval_for_size(
    caps: &gst::Caps,
    width: i32,
    height: i32,
) -> Option<FrameInterval> {
    let mut maximum: Option<FrameInterval> = None;
    for structure in caps.iter() {
        let Ok(structure_width) = structure.get::<i32>("width") else {
            continue;
        };
        let Ok(structure_height) = structure.get::<i32>("height") else {
            continue;
        };
        if structure_width != width || structure_height != height {
            continue;
        }
        let Some(interval) = max_frame_interval_from_structure_framerate(structure) else {
            continue;
        };
        if maximum
            .as_ref()
            .is_none_or(|current| interval.frames_per_second_exceeds(current))
        {
            maximum = Some(interval);
        }
    }
    maximum
}

fn max_frame_interval_from_structure_framerate(
    structure: &gst::StructureRef,
) -> Option<FrameInterval> {
    let sendvalue = structure.value("framerate").ok()?;
    match sendvalue.type_().name() {
        "GstFraction" => sendvalue.get::<gst::Fraction>().ok().map(Into::into),
        "GstFractionRange" => sendvalue
            .get::<gst::FractionRange>()
            .ok()
            .map(|range| range.max().into()),
        "GstValueList" => {
            let list = sendvalue.get::<gst::List>().ok()?;
            list.iter()
                .filter_map(|value| value.get::<gst::Fraction>().ok().map(Into::into))
                .reduce(|left: FrameInterval, right: FrameInterval| {
                    if left.frames_per_second_exceeds(&right) {
                        left
                    } else {
                        right
                    }
                })
        }
        _ => None,
    }
}

fn libcamera_native_fps_only_enabled() -> bool {
    env_flag_enabled(std::env::var(LIBCAMERA_NATIVE_FPS_ONLY_ENV).ok().as_deref())
}

fn env_flag_enabled(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim) else {
        return false;
    };
    value == "1"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("on")
}

fn fastest_frame_interval(intervals: &[FrameInterval]) -> Option<FrameInterval> {
    intervals.iter().cloned().reduce(|left, right| {
        if left.frames_per_second_exceeds(&right) {
            left
        } else {
            right
        }
    })
}

/// Keep only each size's native mode fps (fastest rate gst advertised for that
/// discrete size). Falls back to the fastest listed interval when caps don't
/// expose a matching gint size.
fn keep_native_mode_frame_intervals(formats: &mut [Format], caps: &gst::Caps) {
    for format in formats.iter_mut() {
        for size in &mut format.sizes {
            let native =
                libcamera_max_frame_interval_for_size(caps, size.width as i32, size.height as i32)
                    .or_else(|| fastest_frame_interval(&size.intervals));
            let Some(native) = native else {
                continue;
            };
            size.intervals = vec![native];
        }
        format.sizes.retain(|size| !size.intervals.is_empty());
    }
}

/// Packed Bayer/mono fourccs carry the CSI bit depth (`SRGGB10`, `grbg10le`, `R10`).
/// Processed grey (`GRAY8`) is not a CSI packed depth.
fn bit_depth_from_fourcc(fourcc: &str) -> Option<u32> {
    let uppercase = fourcc.to_ascii_uppercase();
    let without_packed = uppercase.strip_suffix("_CSI2P").unwrap_or(&uppercase);
    let without_endian = without_packed
        .strip_suffix("LE")
        .or_else(|| without_packed.strip_suffix("BE"))
        .unwrap_or(without_packed);
    const PREFIXES: [&str; 10] = [
        "SRGGB", "SBGGR", "SGRBG", "SGBRG", "RGGB", "BGGR", "GRBG", "GBRG", "MONO", "R",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = without_endian.strip_prefix(prefix)
            && let Ok(depth) = rest.parse::<u32>()
            && matches!(depth, 8 | 10 | 12 | 16)
        {
            return Some(depth);
        }
    }
    None
}

fn is_libcamera_bayer_structure(structure: &gst::StructureRef) -> bool {
    match structure.name().as_str() {
        "video/x-bayer" => true,
        "video/x-raw" => structure_fourccs(structure)
            .iter()
            .any(|fourcc| bit_depth_from_fourcc(fourcc).is_some()),
        _ => false,
    }
}

fn size_depth_mut(size: &mut Size, bit_depth: u32) -> &mut SizeDepth {
    if let Some(index) = size
        .depths
        .iter()
        .position(|depth| depth.bit_depth == bit_depth)
    {
        return &mut size.depths[index];
    }
    size.depths.push(SizeDepth {
        bit_depth,
        intervals: Vec::new(),
    });
    let index = size.depths.len() - 1;
    &mut size.depths[index]
}

fn sort_frame_intervals_fastest_first(intervals: &mut [FrameInterval]) {
    intervals.sort_by(|left, right| {
        match (
            left.frames_per_second_exceeds(right),
            right.frames_per_second_exceeds(left),
        ) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    });
}

/// Discrete Bayer sensor modes. GstDevice VideoRecording caps invent the same
/// discrete sizes *without* a framerate; require one when parsing those.
/// `stream-role=raw` StreamFormats have the real sensor sizes and no fps.
fn libcamera_native_mode_sizes(caps: &gst::Caps) -> Vec<Size> {
    libcamera_bayer_mode_sizes(caps, true)
}

fn libcamera_bayer_mode_sizes(caps: &gst::Caps, require_framerate: bool) -> Vec<Size> {
    let mut sizes_by_resolution: BTreeMap<(u32, u32), Size> = BTreeMap::new();
    for structure in caps.iter() {
        if !is_libcamera_bayer_structure(structure) {
            continue;
        }
        let widths = structure_discrete_dimension(structure, "width");
        let heights = structure_discrete_dimension(structure, "height");
        if widths.len() != 1 || heights.len() != 1 {
            continue;
        }
        let width = widths[0];
        let height = heights[0];
        let interval = max_frame_interval_from_structure_framerate(structure);
        if require_framerate && interval.is_none() {
            continue;
        }

        let size = sizes_by_resolution
            .entry((width, height))
            .or_insert_with(|| Size {
                width,
                height,
                intervals: Vec::new(),
                depths: Vec::new(),
            });
        for fourcc in structure_fourccs(structure) {
            let Some(bit_depth) = bit_depth_from_fourcc(&fourcc) else {
                continue;
            };
            let depth = size_depth_mut(size, bit_depth);
            if let Some(interval) = interval
                && depth
                    .intervals
                    .iter()
                    .all(|current| interval.frames_per_second_exceeds(current))
            {
                depth.intervals = vec![interval];
            }
        }
    }
    for size in sizes_by_resolution.values_mut() {
        size.depths.sort_by_key(|depth| depth.bit_depth);
    }
    let mut sizes: Vec<Size> = sizes_by_resolution.into_values().collect();
    sizes.sort();
    sizes.reverse();
    sizes
}

fn structure_fourccs(structure: &gst::StructureRef) -> Vec<String> {
    let Ok(sendvalue) = structure.value("format") else {
        return Vec::new();
    };
    match sendvalue.type_().name() {
        "gchararray" => sendvalue
            .get::<String>()
            .ok()
            .map(|fourcc| vec![fourcc])
            .unwrap_or_default(),
        "GstValueList" => sendvalue
            .get::<gst::List>()
            .ok()
            .map(|list| {
                list.iter()
                    .filter_map(|value| value.get::<String>().ok())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn structure_discrete_dimension(structure: &gst::StructureRef, field: &str) -> Vec<u32> {
    let Ok(sendvalue) = structure.value(field) else {
        return Vec::new();
    };
    match sendvalue.type_().name() {
        "gint" => sendvalue
            .get::<i32>()
            .ok()
            .map(|value| vec![value as u32])
            .unwrap_or_default(),
        "GstValueList" => sendvalue
            .get::<gst::List>()
            .ok()
            .map(|list| {
                list.iter()
                    .filter_map(|value| value.get::<i32>().ok().map(|value| value as u32))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn cached_libcamera_native_sizes(camera_name: &str) -> Option<Vec<Size>> {
    let Ok(cache) = LIBCAMERA_NATIVE_SIZES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        warn!("libcamera native-size cache poisoned");
        return None;
    };
    cache.get(camera_name).cloned()
}

fn store_libcamera_native_sizes(camera_name: &str, sizes: Vec<Size>) {
    let Ok(mut cache) = LIBCAMERA_NATIVE_SIZES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        warn!("libcamera native-size cache poisoned; not storing {camera_name:?}");
        return;
    };
    cache.insert(camera_name.to_string(), sizes);
}

fn cached_device_formats(device_path: &str) -> Option<Vec<Format>> {
    DEVICE_FORMATS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?
        .get(device_path)
        .cloned()
}

fn store_device_formats(device_path: &str, formats: Vec<Format>) {
    let Ok(mut cache) = DEVICE_FORMATS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        warn!("device format cache poisoned; not storing {device_path:?}");
        return;
    };
    cache.insert(device_path.to_string(), formats);
}

fn cached_device_controls(device_path: &str) -> Option<Vec<Control>> {
    DEVICE_CONTROLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?
        .get(device_path)
        .cloned()
}

fn store_device_controls(device_path: &str, controls: Vec<Control>) {
    let Ok(mut cache) = DEVICE_CONTROLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        warn!("device control cache poisoned; not storing {device_path:?}");
        return;
    };
    cache.insert(device_path.to_string(), controls);
}

/// Sensor sizes from `libcamerasrc` `stream-role=raw` StreamFormats, plus max fps
/// from a `sensor-config` pin at each size. GstDevice VideoRecording caps are the
/// ISP menu and cannot supply this.
#[instrument(level = "debug")]
fn probe_libcamera_native_sizes(camera_name: &str) -> Vec<Size> {
    if let Some(sizes) = cached_libcamera_native_sizes(camera_name) {
        return sizes;
    }

    let Some(raw_caps) = probe_libcamera_raw_stream_formats(camera_name) else {
        return Vec::new();
    };
    let mut sizes = libcamera_bayer_mode_sizes(&raw_caps, false);
    fill_libcamera_mode_frame_intervals(camera_name, &mut sizes);
    sizes.retain(|size| size.width > 0 && size.height > 0);
    finalize_libcamera_size_intervals(&mut sizes);

    if !sizes.is_empty() {
        store_libcamera_native_sizes(camera_name, sizes.clone());
    }
    sizes
}

fn fill_libcamera_mode_frame_intervals(camera_name: &str, sizes: &mut [Size]) {
    for size in sizes.iter_mut() {
        for bit_depth in [10, 12, 8] {
            if size
                .depths
                .iter()
                .any(|depth| depth.bit_depth == bit_depth && !depth.intervals.is_empty())
            {
                continue;
            }
            let Some(interval) = probe_libcamera_mode_frame_interval(
                camera_name,
                size.width,
                size.height,
                bit_depth,
            ) else {
                continue;
            };
            size_depth_mut(size, bit_depth).intervals = vec![interval];
        }
        size.depths.retain(|depth| !depth.intervals.is_empty());
        size.depths.sort_by_key(|depth| depth.bit_depth);
        size.intervals.clear();
    }
}

fn finalize_libcamera_size_intervals(sizes: &mut [Size]) {
    for size in sizes {
        for depth in &mut size.depths {
            add_supported_common_frame_intervals(&mut depth.intervals);
        }
    }
}

/// Common rates at or below the fastest native mode, unless
/// `MCM_LIBCAMERA_NATIVE_FPS_ONLY` is set.
fn add_supported_common_frame_intervals(intervals: &mut Vec<FrameInterval>) {
    let Some(native_max) = fastest_frame_interval(intervals) else {
        return;
    };
    if libcamera_native_fps_only_enabled() {
        return;
    }
    for &denominator in DEFAULT_FRAME_INTERVALS {
        let common = FrameInterval {
            numerator: 1,
            denominator,
        };
        if common.frames_per_second_exceeds(&native_max) {
            continue;
        }
        if intervals
            .iter()
            .all(|current| !common.frames_per_second_equals(current))
        {
            intervals.push(common);
        }
    }
    sort_frame_intervals_fastest_first(intervals);
}

/// A live `libcamerasrc` already owns the process CameraManager. Starting
/// another PLAYING probe (same or other camera) races `requestCompleted` and
/// SIGSEGVs gst-libcamera (`wrap->request_.get() == request`).
fn live_libcamerasrc_blocks_format_probe() -> bool {
    !matches!(try_any_live_libcamerasrc(), LiveSourceLookup::NotStreaming)
}

/// Capture the StreamFormats filter `libcamerasrc` sends during negotiate when
/// the pad role is `raw` (actual sensor sizes, not the ISP scaler menu).
#[instrument(level = "debug")]
fn probe_libcamera_raw_stream_formats(camera_name: &str) -> Option<gst::Caps> {
    if live_libcamerasrc_blocks_format_probe() {
        debug!(
            "Skipping libcamera raw-formats probe for {camera_name:?}; a live libcamerasrc is running"
        );
        return None;
    }
    let pipeline = match gst::parse::launch(
        "libcamerasrc name=probe-source ! fakesink name=probe-sink sync=false",
    ) {
        Ok(element) => element,
        Err(error) => {
            warn!("libcamera raw-formats probe pipeline failed to parse: {error}");
            return None;
        }
    };
    let pipeline = pipeline.downcast::<gst::Pipeline>().ok()?;
    let source = pipeline.by_name("probe-source")?;
    let sink = pipeline.by_name("probe-sink")?;
    let src_pad = source.static_pad("src")?;
    if !src_pad.has_property("stream-role") {
        stop_libcamera_probe_pipeline(&pipeline);
        return None;
    }
    src_pad.set_property_from_str("stream-role", "raw");
    source.set_property("camera-name", camera_name);

    let captured = Arc::new(Mutex::new(None::<gst::Caps>));
    let sink_pad = sink.static_pad("sink")?;
    let captured_for_probe = captured.clone();
    sink_pad.add_probe(gst::PadProbeType::QUERY_BOTH, move |_pad, info| {
        if let Some(query) = info.query()
            && let gst::QueryView::Caps(caps_query) = query.view()
            && let Some(filter) = caps_query.filter_owned()
            && !filter.is_any()
            && filter.iter().any(|structure| {
                is_libcamera_bayer_structure(structure)
                    && !structure_discrete_dimension(structure, "width").is_empty()
            })
        {
            let mut guard = captured_for_probe
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let replace = match guard.as_ref() {
                None => true,
                Some(current) => filter.size() > current.size(),
            };
            if replace {
                *guard = Some(filter);
            }
        }
        gst::PadProbeReturn::Ok
    });

    let bus = pipeline.bus()?;
    if let Err(error) = pipeline.set_state(gst::State::Playing) {
        debug!("libcamera raw-formats probe set_state(Playing) failed: {error}");
        stop_libcamera_probe_pipeline(&pipeline);
        return None;
    }

    let deadline = Instant::now() + LIBCAMERA_FORMAT_PROBE_TIMEOUT;
    loop {
        if captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
        {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(50));
        let timeout = gst::ClockTime::from_nseconds(wait.as_nanos() as u64);
        if let Some(message) = bus.timed_pop(timeout)
            && let gst::MessageView::Error(error) = message.view()
        {
            debug!("libcamera raw-formats probe bus error: {}", error.error());
            break;
        }
    }

    let caps = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    stop_libcamera_probe_pipeline(&pipeline);
    if let Some(ref caps) = caps {
        debug!(
            "libcamera raw StreamFormats for {camera_name:?}: {} structure(s)",
            caps.size()
        );
    } else {
        debug!("libcamera raw StreamFormats probe got no Bayer filter for {camera_name:?}");
    }
    caps
}

/// Max fps for a pinned sensor mode.
///
/// `libcamerasrc` reads a *fraction* framerate from peer caps, then clamps it
/// via `FrameDurationLimits`. A capsfilter of `1000/1` advertises that rate and
/// then rejects the clamped caps (NOT_NEGOTIATED). Answer the CAPS query with
/// `1000/1` and let fakesink accept whatever is pushed.
#[instrument(level = "debug")]
fn probe_libcamera_mode_frame_interval(
    camera_name: &str,
    width: u32,
    height: u32,
    bit_depth: u32,
) -> Option<FrameInterval> {
    if live_libcamerasrc_blocks_format_probe() {
        debug!(
            "Skipping libcamera fps probe for {camera_name:?} {width}x{height}@{bit_depth}; a live libcamerasrc is running"
        );
        return None;
    }
    let pipeline = match gst::parse::launch(
        "libcamerasrc name=probe-source ! fakesink name=probe-sink sync=false",
    ) {
        Ok(element) => element,
        Err(error) => {
            debug!("libcamera fps probe pipeline failed to parse: {error}");
            return None;
        }
    };
    let pipeline = pipeline.downcast::<gst::Pipeline>().ok()?;
    let source = pipeline.by_name("probe-source")?;
    let sink = pipeline.by_name("probe-sink")?;
    source.set_property("camera-name", camera_name);
    if let Some(src_pad) = source.static_pad("src")
        && src_pad.has_property("stream-role")
    {
        src_pad.set_property_from_str("stream-role", "video-recording");
    }
    if source.has_property("sensor-config") {
        source.set_property(
            "sensor-config",
            gst::Structure::builder("sensor/config")
                .field("width", width as i32)
                .field("height", height as i32)
                .field("depth", bit_depth as i32)
                .build(),
        );
    }

    let advertised = gst::Caps::builder("video/x-raw")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(1000, 1))
        .build();
    let sink_pad = sink.static_pad("sink")?;
    sink_pad.add_probe(gst::PadProbeType::QUERY_BOTH, move |_pad, info| {
        if let Some(query) = info.query_mut()
            && let gst::QueryViewMut::Caps(caps_query) = query.view_mut()
        {
            caps_query.set_result(Some(&advertised));
            return gst::PadProbeReturn::Handled;
        }
        gst::PadProbeReturn::Ok
    });

    let captured = Arc::new(Mutex::new(None::<FrameInterval>));
    let src_pad = source.static_pad("src")?;
    let captured_for_caps = captured.clone();
    src_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
        if let Some(event) = info.event()
            && let gst::EventView::Caps(caps_event) = event.view()
        {
            for structure in caps_event.caps().iter() {
                if let Some(interval) = max_frame_interval_from_structure_framerate(structure)
                    && !is_unclamped_fps_probe_interval(&interval)
                {
                    let mut guard = captured_for_caps
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if guard.is_none() {
                        *guard = Some(interval);
                    }
                    break;
                }
            }
        }
        gst::PadProbeReturn::Ok
    });
    let captured_for_buffer = captured.clone();
    let previous_pts_ns = Arc::new(Mutex::new(None::<u64>));
    src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
        let Some(buffer) = info.buffer() else {
            return gst::PadProbeReturn::Ok;
        };
        if let Some(duration) = buffer.duration()
            && let Some(interval) = frame_interval_from_nanoseconds(duration.nseconds())
            && !is_unclamped_fps_probe_interval(&interval)
        {
            *captured_for_buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(interval);
            return gst::PadProbeReturn::Ok;
        }
        if let Some(presentation_timestamp) = buffer.pts() {
            let nanoseconds = presentation_timestamp.nseconds();
            let mut previous = previous_pts_ns
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(previous_nanoseconds) = *previous
                && nanoseconds > previous_nanoseconds
                && let Some(interval) =
                    frame_interval_from_nanoseconds(nanoseconds - previous_nanoseconds)
                && !is_unclamped_fps_probe_interval(&interval)
            {
                *captured_for_buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(interval);
            }
            *previous = Some(nanoseconds);
        }
        gst::PadProbeReturn::Ok
    });

    let bus = pipeline.bus()?;
    if let Err(error) = pipeline.set_state(gst::State::Playing) {
        debug!(
            "libcamera fps probe {width}x{height} depth={bit_depth} set_state(Playing) failed: {error}"
        );
        stop_libcamera_probe_pipeline(&pipeline);
        return None;
    }

    let deadline = Instant::now() + LIBCAMERA_FORMAT_PROBE_TIMEOUT;
    loop {
        if captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(interval_is_from_buffer_duration)
        {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(50));
        let timeout = gst::ClockTime::from_nseconds(wait.as_nanos() as u64);
        if let Some(message) = bus.timed_pop(timeout)
            && let gst::MessageView::Error(error) = message.view()
        {
            debug!(
                "libcamera fps probe {width}x{height} depth={bit_depth} bus error: {}",
                error.error()
            );
            break;
        }
    }

    let interval = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    stop_libcamera_probe_pipeline(&pipeline);
    interval
}

fn is_unclamped_fps_probe_interval(interval: &FrameInterval) -> bool {
    if interval.numerator == 0 {
        return false;
    }
    f64::from(interval.denominator) / f64::from(interval.numerator) >= 999.0
}

fn interval_is_from_buffer_duration(interval: &FrameInterval) -> bool {
    interval.denominator == 1_000_000_000
}

fn frame_interval_from_nanoseconds(nanoseconds: u64) -> Option<FrameInterval> {
    if nanoseconds == 0 || nanoseconds > u64::from(u32::MAX) {
        return None;
    }
    Some(FrameInterval {
        numerator: nanoseconds as u32,
        denominator: 1_000_000_000,
    })
}

fn stop_libcamera_probe_pipeline(pipeline: &gst::Pipeline) {
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        warn!("libcamera format probe set_state(Null) failed: {error}");
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while pipeline.current_state() != gst::State::Null && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn get_device_formats_using_gstreamer(
    device_path: &str,
    typ: &VideoSourceLocalType,
) -> Result<Vec<Format>> {
    let device = gst_device_monitor::local_device_with_path(device_path)?;

    let caps = gst_device_monitor::device_caps(&device)?;
    let is_libcamera = matches!(typ, VideoSourceLocalType::Libcamera(_));

    let mut sizes_by_encode: HashMap<VideoEncodeType, HashSet<Size>> = HashMap::new();

    caps.iter().for_each(|structure| {
        let encodes: Vec<VideoEncodeType> = match structure.name().as_str() {
            "video/x-raw" => match structure.value("format") {
                Ok(sendvalue) => match sendvalue.type_().name() {
                    "gchararray" => match sendvalue.get::<String>() {
                        Ok(fourcc) => {
                            vec![VideoEncodeType::from_fourcc(&fourcc)]
                        }
                        Err(error) => {
                            warn!(
                                "Failed reading video/x-raw format as gchararray: {structure:#?}: {error:?}"
                            );
                            return;
                        }
                    },
                    "GstValueList" => match sendvalue.get::<gst::List>() {
                        Ok(list) => list
                            .iter()
                            .filter_map(|v| v.get::<String>().ok())
                            .map(|fourcc| VideoEncodeType::from_fourcc(&fourcc))
                            .collect(),
                        Err(error) => {
                            warn!(
                                "Failed reading video/x-raw format as GstValueList: {structure:#?}: {error:?}"
                            );
                            return;
                        }
                    },
                    unsupported_type => {
                        info!(
                            "video/x-raw format with unsupported type: {unsupported_type:?}: {structure:#?}"
                        );
                        return;
                    }
                },
                Err(error) => {
                    warn!("No format on video/x-raw: {structure:#?}: {error:?}");
                    return;
                }
            },
            "image/jpeg" => vec![VideoEncodeType::Mjpg],
            "video/x-h264" => vec![VideoEncodeType::H264],
            "video/x-h265" => vec![VideoEncodeType::H265],
            "video/x-bayer" if is_libcamera => return,
            other => {
                info!("unknown format: {other:?}");

                return;
            }
        };

        // gst-libcamera also emits StreamFormats::range as GstIntRange (ISP scaler).
        // Discrete sizes are already gint structures; do not sample STANDARD_SIZES.
        if is_libcamera
            && (structure
                .value("width")
                .ok()
                .is_some_and(|value| value.type_().name() == "GstIntRange")
                || structure
                    .value("height")
                    .ok()
                    .is_some_and(|value| value.type_().name() == "GstIntRange"))
        {
            return;
        }

        let mut heights = match structure.value("height") {
            Ok(sendvalue) => match sendvalue.type_().name() {
                "gint" => match sendvalue.get::<i32>() {
                    Ok(value) => vec![value as u32],
                    Err(error) => {
                        warn!("Failed reading height as gint: {structure:#?}: {error:?}");
                        return;
                    }
                },
                "GstIntRange" => {
                    let range = match sendvalue.get::<gst::IntRange<i32>>() {
                        Ok(range) => range,
                        Err(error) => {
                            warn!(
                                "Failed reading height as GstIntRange: {structure:#?}: {error:?}"
                            );
                            return;
                        }
                    };

                    let start = range.min() as u32;
                    let end = range.max() as u32;
                    let step = range.step() as u32;

                    STANDARD_SIZES
                        .iter()
                        .filter_map(|(_, height)| {
                            if height >= &start && height <= &end && (height % step == 0) {
                                return Some(*height);
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                }
                "GstValueList" => match sendvalue.get::<gst::List>() {
                    Ok(list) => list
                        .iter()
                        .filter_map(|v| v.get::<i32>().ok().map(|value| value as u32))
                        .collect::<Vec<_>>(),
                    Err(error) => {
                        warn!("Failed reading height as GstValueList: {structure:#?}: {error:?}");
                        return;
                    }
                },
                unsupported_type => {
                    info!("Height with unsupported type: {unsupported_type:?}, {structure:#?}");
                    return;
                }
            },
            Err(error) => {
                info!("No height: {structure:#?}: {error:?}");
                return;
            }
        };

        let mut widths = match structure.value("width") {
            Ok(sendvalue) => match sendvalue.type_().name() {
                "gint" => match sendvalue.get::<i32>() {
                    Ok(value) => vec![value as u32],
                    Err(error) => {
                        warn!("Failed reading width as gint: {structure:#?}: {error:?}");
                        return;
                    }
                },
                "GstIntRange" => {
                    let range = match sendvalue.get::<gst::IntRange<i32>>() {
                        Ok(range) => range,
                        Err(error) => {
                            warn!("Failed reading width as GstIntRange: {structure:#?}: {error:?}");
                            return;
                        }
                    };

                    let start = range.min() as u32;
                    let end = range.max() as u32;
                    let step = range.step() as u32;

                    STANDARD_SIZES
                        .iter()
                        .filter_map(|(width, _)| {
                            if width >= &start && width <= &end && (width % step == 0) {
                                return Some(*width);
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                }
                "GstValueList" => match sendvalue.get::<gst::List>() {
                    Ok(list) => list
                        .iter()
                        .filter_map(|v| v.get::<i32>().ok().map(|value| value as u32))
                        .collect::<Vec<_>>(),
                    Err(error) => {
                        warn!("Failed reading width as GstValueList: {structure:#?}: {error:?}");
                        return;
                    }
                },
                unsupported_type => {
                    info!("Width with unsupported type: {unsupported_type:?}, {structure:#?}");
                    return;
                }
            },
            Err(error) => {
                info!("No width: {structure:#?}: {error:?}");
                return;
            }
        };

        let mut intervals = match structure.value("framerate") {
            Ok(sendvalue) => match sendvalue.type_().name() {
                "GstFraction" => match sendvalue.get::<gst::Fraction>() {
                    Ok(fraction) => vec![fraction.into()],
                    Err(error) => {
                        warn!(
                            "Failed reading framerate as GstFraction: {structure:#?}: {error:?}"
                        );
                        return;
                    }
                },
                "GstFractionRange" => {
                    let range = match sendvalue.get::<gst::FractionRange>() {
                        Ok(range) => range,
                        Err(error) => {
                            warn!(
                                "Failed reading framerate as GstFractionRange: {structure:#?}: {error:?}"
                            );
                            return;
                        }
                    };

                    compute_intervals_from_range(range.min().into(), range.max().into(), 1)
                }
                "GstValueList" => match sendvalue.get::<gst::List>() {
                    Ok(list) => list
                        .iter()
                        .filter_map(|sendvalue| {
                            sendvalue.get::<gst::Fraction>().ok().map(Into::into)
                        })
                        .collect::<Vec<_>>(),
                    Err(error) => {
                        warn!(
                            "Failed reading framerate as GstValueList: {structure:#?}: {error:?}"
                        );
                        return;
                    }
                },
                unsupported_type => {
                    info!("Framerate with unsupported type: {unsupported_type:?}, {structure:#?}");
                    return;
                }
            },
            Err(error) => {
                if is_libcamera {
                    trace!(
                        "Caps without framerate, not inventing defaults for libcamera: {structure:#?}: {error:?}"
                    );
                    Vec::new()
                } else {
                    trace!(
                        "Caps without framerate, using defaults: {structure:#?}: {error:?}"
                    );
                    DEFAULT_FRAME_INTERVALS
                        .iter()
                        .map(|&denominator| FrameInterval {
                            numerator: 1,
                            denominator,
                        })
                        .collect()
                }
            }
        };

        heights.sort();
        heights.dedup();

        widths.sort();
        widths.dedup();

        intervals.sort();
        intervals.dedup();
        intervals.reverse();

        widths.into_iter().zip(heights).for_each(|(width, height)| {
            let size = Size {
                width,
                height,
                intervals: intervals.clone(),
                depths: Vec::new(),
            };

            for encode in &encodes {
                sizes_by_encode
                    .entry(encode.clone())
                    .or_default()
                    .insert(size.clone());
            }
        });
    });

    let mut formats = Vec::with_capacity(sizes_by_encode.len());

    sizes_by_encode.into_iter().for_each(|(encode, sizes)| {
        let mut sizes = sizes.into_iter().collect::<Vec<Size>>();
        sizes.sort();
        // sizes.dedup();
        sizes.reverse();

        formats.push(Format { encode, sizes })
    });

    if is_libcamera {
        formats.retain(|format| {
            matches!(
                format.encode,
                VideoEncodeType::Nv12 | VideoEncodeType::Rgb | VideoEncodeType::Yuyv
            )
        });
        let native_sizes = {
            let probed = probe_libcamera_native_sizes(device_path);
            if probed.is_empty() {
                libcamera_native_mode_sizes(&caps)
            } else {
                probed
            }
        };
        if !native_sizes.is_empty() {
            if formats.is_empty() {
                for encode in [
                    VideoEncodeType::Nv12,
                    VideoEncodeType::Rgb,
                    VideoEncodeType::Yuyv,
                ] {
                    formats.push(Format {
                        encode,
                        sizes: native_sizes.clone(),
                    });
                }
            } else {
                for format in &mut formats {
                    format.sizes = native_sizes.clone();
                }
            }
        } else if libcamera_native_fps_only_enabled() {
            keep_native_mode_frame_intervals(&mut formats, &caps);
            info!("Keeping only native libcamera mode fps via {LIBCAMERA_NATIVE_FPS_ONLY_ENV}");
        }
        formats.retain(|format| !format.sizes.is_empty());
    }

    Ok(formats)
}

#[instrument(level = "debug")]
fn validate_control(control: &Control, value: i64) -> Result<(), String> {
    if control.state.is_inactive {
        return Err("Control is inactive".to_string());
    } else if control.state.is_disabled {
        return Err("Control is disabled".to_string());
    }

    match &control.configuration {
        ControlType::Slider(control) => {
            if value > control.max {
                return Err(format!(
                    "Value {value:?} is greater than the Control maximum value: {:?}",
                    control.max
                ));
            } else if value < control.min {
                return Err(format!(
                    "Value {value:?} is lower than the Control minimum value: {:?}",
                    control.min
                ));
            }
        }
        ControlType::Menu(control) => {
            if !control.options.iter().any(|opt| opt.value == value) {
                return Err(format!(
                    "Value {value:?} is not one of the Control options: {:?}",
                    control.options
                ));
            }
        }
        ControlType::Bool(_) => {
            let values = &[0, 1];
            if !values.contains(&value) {
                return Err(format!(
                    "Value {value:?} is not one of the Control accepted values: {values:?}"
                ));
            }
        }
        ControlType::Flags(control) => {
            let allowed = control
                .flags
                .iter()
                .fold(0i64, |mask, flag| mask | flag.value);
            if value & !allowed != 0 {
                return Err(format!(
                    "Value {value:?} uses undefined flag bits for control {:?}",
                    control.flags
                ));
            }
        }
    }

    Ok(())
}

impl VideoSourceFormats for VideoSourceLocal {
    #[instrument(level = "debug")]
    async fn formats(&self) -> Vec<Format> {
        let device_path = &self.device_path;
        if let Some(formats) = cached_device_formats(device_path) {
            return formats;
        }
        let typ = &self.typ;

        match get_device_formats_using_gstreamer(device_path, typ) {
            Ok(formats) => {
                if !formats.is_empty() {
                    store_device_formats(device_path, formats.clone());
                }
                formats
            }
            Err(error) => {
                warn!("Failed getting formats for device {device_path:?}: {error:?}");
                vec![]
            }
        }
    }
}

impl VideoSource for VideoSourceLocal {
    #[instrument(level = "debug")]
    fn name(&self) -> &String {
        &self.name
    }

    #[instrument(level = "debug")]
    fn source_string(&self) -> &str {
        &self.device_path
    }

    #[instrument(level = "debug")]
    fn set_control_by_name(&self, control_name: &str, value: i64) -> std::io::Result<()> {
        let Some(control_id) = self
            .controls()
            .iter()
            .find_map(|control| (control.name == control_name).then_some(control.id))
        else {
            let names: Vec<String> = self
                .controls()
                .into_iter()
                .map(|control| control.name)
                .collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Control named {control_name:?} was not found, options are: {names:?}"),
            ));
        };

        self.set_control_by_id(control_id, value)
    }

    #[instrument(level = "debug")]
    fn set_control_by_id(&self, control_id: u64, value: i64) -> std::io::Result<()> {
        if matches!(self.typ, VideoSourceLocalType::Libcamera(_)) {
            let Some(control) =
                super::libcamera_controls::find_control(&self.device_path, control_id)
            else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "Control ID {control_id:?} was not found for libcamera device {:?}",
                        self.device_path
                    ),
                ));
            };
            if let Err(error) = validate_control(&control, value) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("Failed setting {control_id:?} to {value:?}: {error}"),
                ));
            }
            return super::libcamera_controls::set_control_by_name(
                &self.device_path,
                &control.name,
                value,
            );
        }

        let Some(control) = self
            .controls()
            .into_iter()
            .find(|control| control.id == control_id)
        else {
            let ids: Vec<u64> = self.controls().iter().map(|control| control.id).collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Control ID {control_id:?} was not found, options are: {ids:?}"),
            ));
        };

        if let Err(error) = validate_control(&control, value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("Failed setting {control_id:?} to {value:?}: {error}"),
            ));
        }

        let device_path = self.device_path.clone();

        let v4l_device = unpanic(move || v4l::Device::with_path(device_path))?;

        //TODO: we should handle value, value64 and string
        let v4l_control = v4l::Control {
            id: control_id as u32,
            value: v4l::control::Value::Integer(value),
        };

        match unpanic(move || v4l_device.set_control(v4l_control)) {
            ok @ Ok(_) => ok,
            Err(error) => {
                trace!("Failed to set control {control:#?}, error: {error:#?}");
                Err(error)
            }
        }
    }

    fn control_value_by_name(&self, control_name: &str) -> std::io::Result<i64> {
        let Some(control_id) = self
            .controls()
            .iter()
            .find_map(|control| (control.name == control_name).then_some(control.id))
        else {
            let names: Vec<String> = self
                .controls()
                .into_iter()
                .map(|control| control.name)
                .collect();
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Control named {control_name:?} was not found, options are: {names:?}"),
            ));
        };

        self.control_value_by_id(control_id)
    }

    #[instrument(level = "debug")]
    fn control_value_by_id(&self, control_id: u64) -> std::io::Result<i64> {
        if matches!(self.typ, VideoSourceLocalType::Libcamera(_)) {
            return super::libcamera_controls::control_value_by_id(&self.device_path, control_id);
        }

        let device_path = self.device_path.clone();

        let v4l_device = unpanic(move || v4l::Device::with_path(device_path))?;

        let control = unpanic(move || v4l_device.control(control_id as u32))?;

        match control.value {
            v4l::control::Value::Integer(value) => Ok(value),
            v4l::control::Value::Boolean(value) => Ok(value as i64),
            unsupported_type => Err(std::io::Error::other(
                format!("Control type {unsupported_type:?} is not supported.").as_str(),
            )),
        }
    }

    #[instrument(level = "debug")]
    fn controls(&self) -> Vec<Control> {
        if matches!(self.typ, VideoSourceLocalType::Libcamera(_)) {
            return super::libcamera_controls::list_controls(&self.device_path);
        }

        if let Some(mut controls) = cached_device_controls(&self.device_path) {
            for control in &mut controls {
                if let Ok(value) = self.control_value_by_id(control.id) {
                    match &mut control.configuration {
                        ControlType::Bool(bool_control) => bool_control.value = value,
                        ControlType::Slider(slider) => slider.value = value,
                        ControlType::Menu(menu) => menu.value = value,
                        ControlType::Flags(flags) => flags.value = value,
                    }
                }
            }
            return controls;
        }

        let mut controls: Vec<Control> = vec![];

        //TODO: create function to encapsulate device
        let device_path = self.device_path.clone();
        let v4l_device = match unpanic(move || v4l::Device::with_path(device_path)) {
            Ok(device) => device,
            Err(error) => {
                trace!("Faield to get device {:?}: {error:?}", self.device_path);
                return controls;
            }
        };

        let v4l_controls = match unpanic(move || v4l_device.query_controls()) {
            Ok(device) => device,
            Err(error) => {
                trace!(
                    "Faield to get controls for device {:?}: {error:?}",
                    self.device_path
                );
                return controls;
            }
        };

        for v4l_control in v4l_controls {
            let mut control = Control {
                name: v4l_control.name,
                id: v4l_control.id as u64,
                state: ControlState {
                    is_disabled: v4l_control.flags.contains(v4l::control::Flags::DISABLED),
                    is_inactive: v4l_control.flags.contains(v4l::control::Flags::INACTIVE),
                },
                ..Default::default()
            };

            if matches!(v4l_control.typ, v4l::control::Type::CtrlClass) {
                // CtrlClass is not a control, so we are skipping it to avoid any access to it, as it will raise an
                // IO error #13: Permission Denied. To better understand, look for 'V4L2_CTRL_TYPE_CTRL_CLASS' on
                // this doc: https://www.kernel.org/doc/html/v5.5/media/uapi/v4l/vidioc-queryctrl.html#c.v4l2_ctrl_type
                continue;
            }

            let value = match self.control_value_by_id(v4l_control.id as u64) {
                Ok(value) => value,
                Err(error) => {
                    error!(
                        "Failed to get control {:?} ({:?}) from device {:?}: {error:?}",
                        control.name, control.id, &self.device_path
                    );
                    continue;
                }
            };
            let default = v4l_control.default;

            match v4l_control.typ {
                v4l::control::Type::Boolean => {
                    control.cpp_type = "bool".to_string();
                    control.configuration = ControlType::Bool(ControlBool { default, value });
                    controls.push(control);
                }
                v4l::control::Type::Integer | v4l::control::Type::Integer64 => {
                    control.cpp_type = "int64".to_string();
                    control.configuration = ControlType::Slider(ControlSlider {
                        default,
                        value,
                        step: v4l_control.step,
                        max: v4l_control.maximum,
                        min: v4l_control.minimum,
                    });
                    controls.push(control);
                }
                v4l::control::Type::Menu | v4l::control::Type::IntegerMenu => {
                    control.cpp_type = "int32".to_string();
                    if let Some(items) = v4l_control.items {
                        let options = items
                            .iter()
                            .map(|(value, name)| ControlOption {
                                name: match name {
                                    v4l::control::MenuItem::Name(name) => name.clone(),
                                    v4l::control::MenuItem::Value(name) => name.to_string(),
                                },
                                value: *value as i64,
                            })
                            .collect();
                        control.configuration = ControlType::Menu(ControlMenu {
                            default,
                            value,
                            options,
                        });
                        controls.push(control);
                    }
                }
                _ => continue,
            };
        }
        if !controls.is_empty() {
            store_device_controls(&self.device_path, controls.clone());
        }
        controls
    }

    #[instrument(level = "debug")]
    fn is_valid(&self) -> bool {
        !self.device_path.is_empty()
    }

    #[instrument(level = "debug")]
    fn is_shareable(&self) -> bool {
        false
    }
}

impl VideoSourceAvailable for VideoSourceLocal {
    #[instrument(level = "debug")]
    async fn cameras_available() -> Vec<VideoSourceType> {
        gst_device_monitor::local_devices()
            .unwrap_or_default()
            .iter()
            .filter_map(|device_weak| {
                let device = device_weak.upgrade()?;
                let display_name = device.display_name().to_string();
                let properties = device.properties();

                let factory_name = gst_device_monitor::source_factory_name(&device)?;

                let (name, device_path, typ) = match factory_name {
                    "v4l2src" => {
                        let properties = properties?;
                        let device_path = properties.get::<String>("device.path").ok()?;
                        let bus = properties.get::<String>("v4l2.device.bus_info").ok()?;
                        (
                            display_name,
                            device_path,
                            VideoSourceLocalType::from_str(&bus),
                        )
                    }
                    "libcamerasrc" => {
                        // libcamera-gst exposes the camera id as the device's display name
                        // (e.g. "/base/soc/i2c0mux/i2c@1/imx708@1a"); that same string is
                        // what `libcamerasrc camera-name=...` expects. Prefer the friendlier
                        // libcamera Model property for the user-visible name when present.
                        let friendly_name = properties
                            .and_then(|p| p.get::<String>("api.libcamera.Model").ok())
                            .unwrap_or_else(|| display_name.clone());
                        (
                            friendly_name,
                            display_name.clone(),
                            VideoSourceLocalType::Libcamera(display_name),
                        )
                    }
                    other => {
                        debug!("Ignoring device with unsupported source factory: {other:?}");
                        return None;
                    }
                };

                Some(VideoSourceType::Local(VideoSourceLocal {
                    name,
                    device_path,
                    typ,
                }))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::*;

    #[traced_test]
    #[instrument(level = "debug")]
    #[test]
    fn bus_decode() {
        let descriptions = vec![
            (
                // Normal desktop
                VideoSourceLocalType::Usb("usb-0000:08:00.3-1".into()),
                "usb-0000:08:00.3-1",
            ),
            (
                // Normal desktop with additional hubs
                VideoSourceLocalType::Usb("usb-0000:08:00.3-2.1".into()),
                "usb-0000:08:00.3-2.1",
            ),
            (
                // Provided by the raspberry pi with a USB camera
                VideoSourceLocalType::Usb("usb-3f980000.usb-1.4".into()),
                "usb-3f980000.usb-1.4",
            ),
            (
                // Provided by the raspberry pi with a Raspberry Pi camera when in to use legacy camera mode
                VideoSourceLocalType::LegacyRpiCam("platform:bcm2835-v4l2-0".into()),
                "platform:bcm2835-v4l2-0",
            ),
            (
                // Sanity test
                VideoSourceLocalType::Unknown("potato".into()),
                "potato",
            ),
        ];

        for description in descriptions {
            assert_eq!(description.0, VideoSourceLocalType::from_str(description.1));
        }
    }
}

#[cfg(test)]
mod device_identification_tests {
    use tracing_test::traced_test;

    use super::*;
    use crate::{
        stream::types::{CaptureConfiguration, StreamInformation},
        video_stream::types::VideoAndStreamInformation,
    };
    use VideoEncodeType::*;

    #[instrument(level = "debug")]
    fn add_available_camera(name: &str, device_path: &str, usb_bus: &str) -> VideoSourceType {
        VideoSourceType::Local(VideoSourceLocal {
            name: name.into(),
            device_path: device_path.into(),
            typ: VideoSourceLocalType::Usb(usb_bus.into()),
        })
    }

    #[instrument(level = "debug")]
    fn formats_for(encodes: &[VideoEncodeType]) -> Vec<Format> {
        encodes
            .iter()
            .cloned()
            .map(|encode| Format {
                encode,
                sizes: vec![Size {
                    width: 1920,
                    height: 1080,
                    intervals: vec![FrameInterval {
                        numerator: 30,
                        denominator: 1,
                    }],
                    depths: Vec::new(),
                }],
            })
            .collect()
    }

    #[instrument(level = "debug")]
    fn create_stream(
        name: &str,
        device_path: &str,
        usb_bus: &str,
        encode: VideoEncodeType,
    ) -> VideoAndStreamInformation {
        VideoAndStreamInformation {
            name: "dummy stream".into(),
            stream_information: StreamInformation {
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    source_encode: encode.clone(),
                    sink_encode: encode,
                    height: 1080,
                    width: 1920,
                    frame_interval: FrameInterval {
                        numerator: 30,
                        denominator: 1,
                    },
                    bit_depth: None,
                    source_configuration: crate::stream::types::SourceConfiguration::Classic,
                    auto_restart_on_config_change: false,
                }),
                endpoints: vec![url::Url::parse("udp://0.0.0.0:5600").unwrap()],
                extended_configuration: None,
            },
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: name.into(),
                device_path: device_path.into(),
                typ: VideoSourceLocalType::Usb(usb_bus.into()),
            }),
        }
    }

    #[traced_test]
    #[tokio::test]
    async fn test_get_cameras_with_same_name() {
        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0"),
            add_available_camera("A", "/dev/video1", "usb_port_0"),
            add_available_camera("A", "/dev/video2", "usb_port_1"),
            add_available_camera("A", "/dev/video3", "usb_port_1"),
            add_available_camera("B", "/dev/video4", "usb_port_2"),
            add_available_camera("B", "/dev/video5", "usb_port_2"),
        ];

        let same_name_candidates = VideoSourceLocal::get_cameras_with_same_name(&candidates, "A");
        assert_eq!(candidates[..4].to_vec(), same_name_candidates);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_get_cameras_with_same_encode() {
        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0"),
            add_available_camera("B", "/dev/video1", "usb_port_1"),
            add_available_camera("C", "/dev/video2", "usb_port_0"),
            add_available_camera("D", "/dev/video3", "usb_port_1"),
        ];

        let formats = HashMap::from([
            ("/dev/video0".into(), formats_for(&[H264])),
            ("/dev/video1".into(), formats_for(&[H264])),
            ("/dev/video2".into(), formats_for(&[Yuyv, Mjpg])),
            ("/dev/video3".into(), formats_for(&[Yuyv, Mjpg])),
        ]);

        let same_encode_candidates =
            VideoSourceLocal::get_cameras_with_same_encode(&candidates, &H264, &formats);
        assert_eq!(candidates[..2].to_vec(), same_encode_candidates);
    }

    #[traced_test]
    #[tokio::test]
    async fn test_get_cameras_with_same_bus() {
        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0"),
            add_available_camera("B", "/dev/video1", "usb_port_0"),
            add_available_camera("C", "/dev/video2", "usb_port_1"),
            add_available_camera("D", "/dev/video3", "usb_port_1"),
        ];

        let same_encode_candidates = VideoSourceLocal::get_cameras_with_same_bus(
            &candidates,
            &VideoSourceLocalType::Usb("usb_port_0".into()),
        );
        assert_eq!(candidates[..2].to_vec(), same_encode_candidates);
    }

    #[traced_test]
    #[tokio::test]
    async fn identify_a_candidate_with_same_name_and_encode() {
        let candidates = vec![
            add_available_camera("A", "/dev/video0", "usb_port_0"),
            add_available_camera("A", "/dev/video1", "usb_port_0"),
            add_available_camera("B", "/dev/video2", "usb_port_1"),
            add_available_camera("B", "/dev/video3", "usb_port_1"),
            add_available_camera("C", "/dev/video3", "usb_port_1"),
        ];
        let formats = HashMap::from([
            ("/dev/video0".into(), formats_for(&[H264])),
            ("/dev/video1".into(), formats_for(&[Yuyv, Mjpg])),
            ("/dev/video2".into(), formats_for(&[H264])),
            // /dev/video3 is shared between B and C in the original test; only the encodes
            // for B's entry are exercised here (C is filtered out by name first).
            ("/dev/video3".into(), formats_for(&[Yuyv, Mjpg])),
        ]);

        let stream = create_stream("A", "/dev/video0", "usb_port_0", H264);
        let (VideoSourceType::Local(source), CaptureConfiguration::Video(capture_configuration)) = (
            &stream.video_source,
            &stream.stream_information.configuration,
        ) else {
            unreachable!("Wrong setup")
        };

        let Ok(Some(candidate_source_string)) = source
            .to_owned()
            .try_identify_device(capture_configuration, &candidates, &formats)
            .await
        else {
            panic!("Failed to identify the only device with the same name and encode")
        };

        assert_eq!(
            candidate_source_string,
            stream.video_source.inner().source_string().to_string()
        );

        // If we remove the only device with the same name and encode, we should get an error
        source
            .to_owned()
            .try_identify_device(capture_configuration, &candidates[1..], &formats)
            .await
            .expect_err("Failed to identify the only device with the same name and encode");
    }

    #[traced_test]
    #[tokio::test]
    async fn identify_a_candidate_when_usb_port_changed() {
        // Before this boot, the device candidates[0] was in "usb_port_0" and the device candidates[1] was in "usb_port_1":
        let last_usb_bus = "usb_port_1";
        let current_usb_bus = "usb_port_0";

        let candidates = vec![
            add_available_camera("A", "/dev/video0", current_usb_bus),
            add_available_camera("A", "/dev/video1", current_usb_bus),
            add_available_camera("B", "/dev/video2", "usb_port_3"),
            add_available_camera("B", "/dev/video3", "usb_port_3"),
        ];
        let formats = HashMap::from([
            ("/dev/video0".into(), formats_for(&[H264])),
            ("/dev/video1".into(), formats_for(&[Yuyv, Mjpg])),
            ("/dev/video2".into(), formats_for(&[H264])),
            ("/dev/video3".into(), formats_for(&[Yuyv, Mjpg])),
        ]);

        for n in (0..3).collect::<Vec<_>>() {
            let stream = create_stream("A", &format!("/dev/video{n}"), last_usb_bus, H264);
            let (
                VideoSourceType::Local(source),
                CaptureConfiguration::Video(capture_configuration),
            ) = (
                &stream.video_source,
                &stream.stream_information.configuration,
            )
            else {
                unreachable!("Wrong setup")
            };

            let Ok(Some(candidate_source_string)) = source
                .to_owned()
                .try_identify_device(capture_configuration, &candidates, &formats)
                .await
            else {
                panic!("Failed to identify the only device with the same name and encode")
            };
            assert_eq!(
                candidate_source_string,
                candidates[0].inner().source_string()
            );

            // If we remove the only device with the same name and encode, we should get an error
            let mut other_candidates = candidates.clone();
            other_candidates.remove(0);
            source
                .to_owned()
                .try_identify_device(capture_configuration, &other_candidates, &formats)
                .await
                .expect_err("Failed to identify the only device with the same name and encode");
        }
    }

    #[traced_test]
    #[tokio::test]
    async fn identify_a_candidate_when_path_changed() {
        // Before this boot, the device candidates[0] was in "/dev/video1" and the device candidates[1] was in "/dev/video0":
        let last_path = "/dev/video1";
        let current_path = "/dev/video0";

        let candidates = vec![
            add_available_camera("A", current_path, "usb_port_0"),
            add_available_camera("A", last_path, "usb_port_1"),
            add_available_camera("A", "/dev/video3", "usb_port_0"),
            add_available_camera("A", "/dev/video5", "usb_port_1"),
        ];
        let formats = HashMap::from([
            (current_path.into(), formats_for(&[H264])),
            (last_path.into(), formats_for(&[H264])),
            ("/dev/video3".into(), formats_for(&[Yuyv, Mjpg])),
            ("/dev/video5".into(), formats_for(&[Yuyv, Mjpg])),
        ]);

        for n in (0..=1).collect::<Vec<_>>() {
            let stream = create_stream("A", last_path, &format!("usb_port_{n}"), H264);

            let (
                VideoSourceType::Local(source),
                CaptureConfiguration::Video(capture_configuration),
            ) = (
                &stream.video_source,
                &stream.stream_information.configuration,
            )
            else {
                unreachable!("Wrong setup")
            };

            let Ok(Some(candidate_source_string)) = source
                .to_owned()
                .try_identify_device(capture_configuration, &candidates, &formats)
                .await
            else {
                panic!("Failed to identify the only device with the same name and encode")
            };
            assert_eq!(
                candidate_source_string,
                candidates[n].inner().source_string()
            );
        }
    }

    #[traced_test]
    #[tokio::test]
    async fn do_not_identify_if_several_devices_with_same_name_and_encode() {
        // Before this boot, the device candidates[0] was in "usb_port_0" and the device candidates[1] was in "usb_port_1":
        let last_usb_bus = "usb_port_1";
        let current_usb_bus = "usb_port_0";

        let candidates = vec![
            add_available_camera("A", "/dev/video0", current_usb_bus),
            add_available_camera("A", "/dev/video1", current_usb_bus),
            add_available_camera("A", "/dev/video4", "usb_port_2"),
            add_available_camera("A", "/dev/video5", "usb_port_2"),
        ];
        let formats = HashMap::from([
            ("/dev/video0".into(), formats_for(&[H264])),
            ("/dev/video1".into(), formats_for(&[Yuyv, Mjpg])),
            ("/dev/video4".into(), formats_for(&[H264])),
            ("/dev/video5".into(), formats_for(&[Yuyv, Mjpg])),
        ]);

        for n in (0..5).collect::<Vec<_>>() {
            let stream = create_stream("A", &format!("/dev/video{n}"), last_usb_bus, H264);
            let (
                VideoSourceType::Local(source),
                CaptureConfiguration::Video(capture_configuration),
            ) = (
                &stream.video_source,
                &stream.stream_information.configuration,
            )
            else {
                unreachable!("Wrong setup")
            };

            assert!(
                source
                    .to_owned()
                    .try_identify_device(capture_configuration, &candidates, &formats)
                    .await
                    .expect("Failed to identify the only device with the same name and encode")
                    .is_none()
            )
        }
    }
}

#[cfg(test)]
mod libcamera_mode_fps_tests {
    use super::*;
    use std::str::FromStr;

    fn frame_interval(numerator: u32, denominator: u32) -> FrameInterval {
        FrameInterval {
            numerator,
            denominator,
        }
    }

    fn listed_bit_depths(size: &Size) -> Vec<u32> {
        size.depths.iter().map(|depth| depth.bit_depth).collect()
    }

    #[test]
    fn env_flag_enabled_accepts_common_truthy_values() {
        assert!(env_flag_enabled(Some("1")));
        assert!(env_flag_enabled(Some("true")));
        assert!(env_flag_enabled(Some(" YES ")));
        assert!(env_flag_enabled(Some("on")));
        assert!(env_flag_enabled(Some("On")));
        assert!(!env_flag_enabled(Some("0")));
        assert!(!env_flag_enabled(Some("false")));
        assert!(!env_flag_enabled(None));
        assert!(!env_flag_enabled(Some("")));
    }

    #[test]
    fn keep_native_mode_frame_intervals_drops_padded_rates_per_size() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", 640i32)
            .field("height", 480i32)
            .field(
                "framerate",
                gst::List::new([
                    gst::Fraction::new(60, 1),
                    gst::Fraction::new(90, 1),
                    gst::Fraction::new(120, 1),
                    gst::Fraction::new(20665, 100),
                ]),
            )
            .build();
        let mut formats = vec![Format {
            encode: VideoEncodeType::Yuyv,
            sizes: vec![Size {
                width: 640,
                height: 480,
                intervals: vec![
                    frame_interval(100, 20665),
                    frame_interval(1, 120),
                    frame_interval(1, 90),
                    frame_interval(1, 60),
                    frame_interval(1, 30),
                ],
                depths: Vec::new(),
            }],
        }];

        keep_native_mode_frame_intervals(&mut formats, &caps);

        assert_eq!(formats[0].sizes.len(), 1);
        assert_eq!(formats[0].sizes[0].intervals.len(), 1);
        let native = &formats[0].sizes[0].intervals[0];
        assert!(native.frames_per_second_equals(&frame_interval(100, 20665)));
    }

    #[test]
    fn keep_native_mode_frame_intervals_falls_back_to_fastest_listed() {
        gst::init().unwrap();
        let caps = gst::Caps::new_empty();
        let mut formats = vec![Format {
            encode: VideoEncodeType::Yuyv,
            sizes: vec![
                Size {
                    width: 1920,
                    height: 1080,
                    intervals: vec![
                        frame_interval(100, 4757),
                        frame_interval(1, 30),
                        frame_interval(1, 24),
                    ],
                    depths: Vec::new(),
                },
                Size {
                    width: 3280,
                    height: 2464,
                    intervals: vec![
                        frame_interval(100, 2119),
                        frame_interval(1, 20),
                        frame_interval(1, 15),
                    ],
                    depths: Vec::new(),
                },
            ],
        }];

        keep_native_mode_frame_intervals(&mut formats, &caps);

        assert!(
            formats[0].sizes[0].intervals[0].frames_per_second_equals(&frame_interval(100, 4757))
        );
        assert!(
            formats[0].sizes[1].intervals[0].frames_per_second_equals(&frame_interval(100, 2119))
        );
    }

    #[test]
    fn pixel_array_size_reads_gst_value_array() {
        gst::init().unwrap();
        let properties = gst::Structure::builder("camera-properties")
            .field(
                "api.libcamera.PixelArraySize",
                gst::Array::new([3280i32, 2464i32]),
            )
            .build();
        assert_eq!(libcamera_pixel_array_size(&properties), Some((3280, 2464)));
    }

    #[test]
    fn max_frame_interval_for_size_uses_fraction_range_max() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", 3280i32)
            .field("height", 2464i32)
            .field(
                "framerate",
                gst::FractionRange::new(gst::Fraction::new(1, 1), gst::Fraction::new(2119, 100)),
            )
            .build();
        let maximum = libcamera_max_frame_interval_for_size(&caps, 3280, 2464).unwrap();
        assert_eq!(maximum, frame_interval(100, 2119));
    }

    #[test]
    fn max_frame_interval_for_size_uses_fastest_list_entry() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", 640i32)
            .field("height", 480i32)
            .field(
                "framerate",
                gst::List::new([
                    gst::Fraction::new(60, 1),
                    gst::Fraction::new(90, 1),
                    gst::Fraction::new(20665, 100),
                ]),
            )
            .build();
        let maximum = libcamera_max_frame_interval_for_size(&caps, 640, 480).unwrap();
        let expected = frame_interval(100, 20665);
        assert!(!maximum.frames_per_second_exceeds(&expected));
        assert!(!expected.frames_per_second_exceeds(&maximum));
        assert!(frame_interval(1, 120).frames_per_second_exceeds(&frame_interval(100, 2119)));
        assert!(libcamera_max_frame_interval_for_size(&caps, 3280, 2464).is_none());
    }

    #[test]
    fn bit_depth_from_fourcc_reads_packed_bayer_and_ignores_yuv() {
        assert_eq!(bit_depth_from_fourcc("SRGGB10"), Some(10));
        assert_eq!(bit_depth_from_fourcc("SRGGB8_CSI2P"), Some(8));
        assert_eq!(bit_depth_from_fourcc("SBGGR12"), Some(12));
        assert_eq!(bit_depth_from_fourcc("R10"), Some(10));
        assert_eq!(bit_depth_from_fourcc("rggb10le"), Some(10));
        assert_eq!(bit_depth_from_fourcc("grbg10le"), Some(10));
        assert_eq!(bit_depth_from_fourcc("NV12"), None);
        assert_eq!(bit_depth_from_fourcc("YUY2"), None);
        assert_eq!(bit_depth_from_fourcc("BGR888"), None);
        assert_eq!(bit_depth_from_fourcc("GRAY8"), None);
        assert_eq!(bit_depth_from_fourcc("GRAY16_LE"), None);
    }

    #[test]
    fn add_supported_common_frame_intervals_keeps_native_and_adds_slower_defaults() {
        let mut intervals = vec![frame_interval(100, 2119)];
        add_supported_common_frame_intervals(&mut intervals);
        let listed: Vec<(u32, u32)> = intervals
            .iter()
            .map(|interval| (interval.numerator, interval.denominator))
            .collect();
        assert_eq!(listed, vec![(100, 2119), (1, 16), (1, 10), (1, 5)]);
    }

    #[test]
    fn add_supported_common_frame_intervals_is_per_depth_native_max() {
        let mut eight_bit = vec![frame_interval(100, 8370)];
        add_supported_common_frame_intervals(&mut eight_bit);
        let eight_listed: Vec<(u32, u32)> = eight_bit
            .iter()
            .map(|interval| (interval.numerator, interval.denominator))
            .collect();
        assert_eq!(
            eight_listed,
            vec![
                (100, 8370),
                (1, 60),
                (1, 30),
                (1, 24),
                (1, 16),
                (1, 10),
                (1, 5)
            ]
        );

        let mut ten_bit = vec![frame_interval(100, 4185)];
        add_supported_common_frame_intervals(&mut ten_bit);
        let ten_listed: Vec<(u32, u32)> = ten_bit
            .iter()
            .map(|interval| (interval.numerator, interval.denominator))
            .collect();
        assert_eq!(
            ten_listed,
            vec![(100, 4185), (1, 30), (1, 24), (1, 16), (1, 10), (1, 5)]
        );
    }

    #[test]
    fn frame_interval_from_nanoseconds_preserves_fractional_fps() {
        let interval = frame_interval_from_nanoseconds(47_192_000).unwrap();
        let fps = f64::from(interval.denominator) / f64::from(interval.numerator);
        assert!((fps - 21.19).abs() < 0.01);
        assert!(interval_is_from_buffer_duration(&interval));
    }

    #[test]
    fn libcamera_native_mode_sizes_uses_bayer_modes_not_isp_yuv() {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(concat!(
            "video/x-raw, format=(string)NV12, width=(int)3200, height=(int)2400; ",
            "video/x-raw, format=(string)GRAY8, width=(int)3200, height=(int)2400; ",
            "video/x-raw, format=(string)YUY2, width=(int)1920, height=(int)1080; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)3280, height=(int)2464, ",
            "framerate=(fraction)2119/100; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)1920, height=(int)1080, ",
            "framerate=(fraction)4757/100; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)1640, height=(int)1232, ",
            "framerate=(fraction)4185/100; ",
            "video/x-bayer, format=(string)rggb8le, width=(int)1640, height=(int)1232, ",
            "framerate=(fraction)4185/100; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)640, height=(int)480, ",
            "framerate=(fraction)20665/100"
        ))
        .unwrap();

        let sizes = libcamera_native_mode_sizes(&caps);
        let listed: Vec<(u32, u32)> = sizes.iter().map(|size| (size.width, size.height)).collect();
        assert_eq!(
            listed,
            vec![(3280, 2464), (1920, 1080), (1640, 1232), (640, 480)]
        );
        assert!(!listed.contains(&(3200, 2400)));
        assert!(sizes.iter().all(|size| size.intervals.is_empty()));
        assert_eq!(listed_bit_depths(&sizes[0]), vec![10]);
        assert!(
            sizes[0].depths[0].intervals[0].frames_per_second_equals(&frame_interval(100, 2119))
        );
        assert_eq!(listed_bit_depths(&sizes[2]), vec![8, 10]);
        assert!(
            sizes[3].depths[0].intervals[0].frames_per_second_equals(&frame_interval(100, 20665))
        );
    }

    #[test]
    fn libcamera_bayer_mode_sizes_accepts_raw_stream_formats_without_fps() {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(concat!(
            "video/x-bayer, format=(string)rggb10le, width=(int)3280, height=(int)2464; ",
            "video/x-bayer, format=(string)rggb8le, width=(int)3280, height=(int)2464; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)1920, height=(int)1080; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)1640, height=(int)1232; ",
            "video/x-bayer, format=(string)rggb8le, width=(int)1640, height=(int)1232; ",
            "video/x-bayer, format=(string)rggb10le, width=(int)640, height=(int)480; ",
            "video/x-raw, format=(string)NV12, width=(int)3200, height=(int)2400"
        ))
        .unwrap();

        assert!(libcamera_native_mode_sizes(&caps).is_empty());
        let sizes = libcamera_bayer_mode_sizes(&caps, false);
        let listed: Vec<(u32, u32)> = sizes.iter().map(|size| (size.width, size.height)).collect();
        assert_eq!(
            listed,
            vec![(3280, 2464), (1920, 1080), (1640, 1232), (640, 480)]
        );
        assert!(sizes.iter().all(|size| size.intervals.is_empty()));
        assert!(
            sizes
                .iter()
                .all(|size| size.depths.iter().all(|depth| depth.intervals.is_empty()))
        );
        assert_eq!(listed_bit_depths(&sizes[0]), vec![8, 10]);
        assert_eq!(listed_bit_depths(&sizes[1]), vec![10]);
        assert_eq!(listed_bit_depths(&sizes[2]), vec![8, 10]);
        assert_eq!(listed_bit_depths(&sizes[3]), vec![10]);
    }

    #[test]
    fn unclamped_fps_probe_interval_is_exactly_1000fps() {
        assert!(is_unclamped_fps_probe_interval(&frame_interval(1, 1000)));
        assert!(!is_unclamped_fps_probe_interval(&frame_interval(1, 21)));
        assert!(!is_unclamped_fps_probe_interval(&frame_interval(100, 2119)));
        let from_gst: FrameInterval = gst::Fraction::new(1000, 1).into();
        assert!(is_unclamped_fps_probe_interval(&from_gst));
        let from_buffer = frame_interval_from_nanoseconds(1_000_000).unwrap();
        assert!(is_unclamped_fps_probe_interval(&from_buffer));
    }
}
