use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tokio::sync::mpsc;
use tracing::*;

use crate::{
    stream::types::{CaptureConfiguration, VideoCaptureConfiguration},
    video::types::{FrameInterval, VideoEncodeType},
};

#[derive(Debug)]
pub struct PluginRankConfig {
    pub name: String,
    pub rank: gst::Rank,
}

#[derive(Debug, Clone)]
pub struct PluginRequirement {
    pub name: String,
    pub min_version: Option<String>,
    pub required: bool,
}

impl PluginRequirement {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            min_version: None,
            required: false,
        }
    }

    pub fn new_with_version(name: &str, min_version: Option<&str>, required: bool) -> Self {
        Self {
            name: name.to_owned(),
            min_version: min_version.map(str::to_owned),
            required,
        }
    }
}

pub fn check_all_plugins() -> Result<()> {
    let plugins_requirements = vec![
        // Basics
        PluginRequirement::new("capsfilter"),
        PluginRequirement::new("videoconvert"),
        PluginRequirement::new("timeoverlay"),
        PluginRequirement::new("queue"),
        PluginRequirement::new("tee"),
        PluginRequirement::new("identity"),
        // Parsers
        PluginRequirement::new("h264parse"),
        PluginRequirement::new("h265parse"),
        // RTP
        PluginRequirement::new("rtph264pay"),
        PluginRequirement::new("rtph265pay"),
        PluginRequirement::new("rtpvrawpay"),
        PluginRequirement::new("rtpjpegpay"),
        // Encoders
        PluginRequirement::new("jpegenc"),
        #[cfg(target_os = "windows")]
        PluginRequirement::new("mfh264enc"),
        #[cfg(not(target_os = "windows"))]
        PluginRequirement::new("x264enc"),
        #[cfg(target_os = "macos")]
        PluginRequirement::new("vtenc_h265"),
        #[cfg(target_os = "windows")]
        PluginRequirement::new("mfh265enc"),
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        PluginRequirement::new("x265enc"),
        // Decoders
        PluginRequirement::new("avdec_h264"),
        PluginRequirement::new("avdec_h265"),
        PluginRequirement::new("jpegdec"),
        // Sources
        PluginRequirement::new("proxysrc"),
        PluginRequirement::new("shmsrc"),
        PluginRequirement::new("v4l2src"),
        PluginRequirement::new("videotestsrc"),
        PluginRequirement::new("qrtimestampsrc"), // Skipping this one as it is checked in runtime when required
        PluginRequirement::new("rtspsrc"),
        // Sinks
        PluginRequirement::new("proxysink"),
        PluginRequirement::new("shmsink"),
        PluginRequirement::new("multiudpsink"),
        PluginRequirement::new("appsink"),
        PluginRequirement::new("webrtcbin"),
    ];

    let mut missing_required = Vec::new();

    for plugin_requirement in &plugins_requirements {
        let PluginRequirement {
            name,
            min_version,
            required,
        } = plugin_requirement;

        let requirement_display = format!(
            "{name}{}",
            min_version
                .as_ref()
                .map_or(String::new(), |v| format!(">={v}"))
        );

        let available = is_gst_plugin_available(plugin_requirement);

        match (available, *required) {
            (false, true) => {
                missing_required.push(requirement_display);
            }
            (false, false) => {
                warn!("Optional GStreamer plugin requirement {requirement_display} is not met: Some features may not be available");
            }
            _ => (),
        }
    }

    if !missing_required.is_empty() {
        return Err(anyhow!(
            "The following GStreamer plugin requirements are not met: {missing_required:#?}"
        ));
    }

    Ok(())
}

pub fn is_gst_plugin_available(
    PluginRequirement {
        name,
        min_version,
        required: _,
    }: &PluginRequirement,
) -> bool {
    // reference: https://github.com/GStreamer/gst/blob/b4ca58df7624b005a33e182a511904d7cceea890/tools/gst-inspect.c#L2148

    if let Err(error) = gst::init() {
        tracing::error!("Error! {error}");
    }

    if let Some(min_version) = min_version {
        let version = semver::Version::parse(min_version).unwrap();
        gst::Registry::get().check_feature_version(
            name,
            version.major.try_into().unwrap(),
            version.minor.try_into().unwrap(),
            version.patch.try_into().unwrap(),
        )
    } else {
        gst::Registry::get().lookup_feature(name).is_some()
    }
}

pub fn set_plugin_rank(plugin_name: &str, rank: gst::Rank) -> Result<()> {
    if let Err(error) = gst::init() {
        tracing::error!("Error! {error}");
    }

    if let Some(feature) = gst::Registry::get().lookup_feature(plugin_name) {
        feature.set_rank(rank);
    } else {
        return Err(anyhow!(
            "Cannot found Gstreamer feature {plugin_name:#?} in the registry.",
        ));
    }

    Ok(())
}

pub fn wait_for_element_state<T: IsA<gst::Element>>(
    element_weak: gst::glib::WeakRef<T>,
    state: gst::State,
    polling_time_millis: u64,
    timeout_time_secs: u64,
) -> Result<()> {
    let mut trials = 1000 * timeout_time_secs / polling_time_millis;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(polling_time_millis));

        let Some(element) = element_weak.upgrade() else {
            return Err(anyhow!("Cannot access Element"));
        };

        if element.current_state() == state {
            break;
        }

        trials = trials.saturating_sub(1);
        if trials == 0 {
            return Err(anyhow!(
                "set state timed-out ({timeout_time_secs:?} seconds)"
            ));
        }
    }

    Ok(())
}

pub fn wait_for_element_state_sync(
    element: &gst::Element,
    state: gst::State,
    polling_time_millis: u64,
    timeout_time_secs: u64,
) -> Result<()> {
    let mut trials = 1000 * timeout_time_secs / polling_time_millis;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(polling_time_millis));

        if element.current_state() == state {
            break;
        }

        trials = trials.saturating_sub(1);
        if trials == 0 {
            return Err(anyhow!(
                "set state timed-out ({timeout_time_secs:?} seconds)"
            ));
        }
    }

    Ok(())
}

pub async fn wait_for_element_state_async(
    element_weak: gst::glib::WeakRef<gst::Pipeline>,
    state: gst::State,
    polling_time_millis: u64,
    timeout_time_secs: u64,
) -> Result<()> {
    let mut trials = 1000 * timeout_time_secs / polling_time_millis;

    let mut period = tokio::time::interval(tokio::time::Duration::from_millis(polling_time_millis));

    loop {
        period.tick().await;

        let Some(element) = element_weak.upgrade() else {
            return Err(anyhow!("Cannot access Element"));
        };

        if element.current_state() == state {
            break;
        }

        trials = trials.saturating_sub(1);
        if trials == 0 {
            return Err(anyhow!(
                "set state timed-out ({timeout_time_secs:?} seconds)"
            ));
        }
    }

    Ok(())
}

fn make_source_description_from_stream_uri(stream_uri: &url::Url) -> Result<String> {
    match stream_uri.scheme() {
        "rtsp" => {
            Ok(format!(
                "rtspsrc name=source location={stream_uri} is-live=true latency=0 do-retransmission=false"
            ))
        }
        "udp" => {
            Ok(format!(
                "udpsrc name=source address={address} port={port} auto-multicast=true do-timestamp=true",
                address = stream_uri.host().context("URI without host")?,
                port = stream_uri.port().context("URI without port")?,
            ))
        }
        unsupported => {
            Err(anyhow!(
                "Scheme {unsupported:#?} is not supported for Redirect Pipelines"
            ))
        }
    }
}

#[instrument(level = "debug", fields(stream_uri = %stream_uri))]
pub async fn get_encode_from_stream_uri(stream_uri: &url::Url) -> Result<VideoEncodeType> {
    let mut description = make_source_description_from_stream_uri(stream_uri)?;

    description.push_str(concat!(
        " ! application/x-rtp, media=(string)video",
        " ! fakesink name=sink sync=false async=false",
    ));

    let pipeline = gst::parse::launch(&description)
        .context("Failed to create pipeline")?
        .downcast::<gst::Pipeline>()
        .expect("Pipeline is not a valid gst::Pipeline");

    let sink = pipeline
        .by_name("sink")
        .context("description without sink")?;

    let (tx, rx) = mpsc::channel(100);

    let sink_pad = sink.static_pad("sink").context("no sink pad")?;
    let probe_id = setup_pad_and_probe(&sink_pad, tx.clone()).context("Failed to add probe")?;

    if let Err(error) = pipeline.set_state(gst::State::Playing) {
        return Err(anyhow!(
            "Failed setting Pipeline state to Playing. Reason: {error:?}"
        ));
    } else if let Err(error) =
        wait_for_element_state_async(pipeline.downgrade(), ::gst::State::Playing, 100, 30).await
    {
        let _ = pipeline.set_state(::gst::State::Null);
        error!("Failed setting Pipeline state to Playing. Reason: {error:?}");
    }

    let encode =
        tokio::time::timeout(tokio::time::Duration::from_secs(3), wait_for_encode(rx)).await;

    sink_pad.remove_probe(probe_id);

    teardown_probe_pipeline(pipeline).await;

    encode?.context("Not found")
}

#[instrument(level = "debug", skip_all)]
async fn wait_for_encode(mut rx: mpsc::Receiver<gst::Caps>) -> Option<VideoEncodeType> {
    #[instrument(level = "debug", skip(structure))]
    async fn parse_structure(structure: &gst::StructureRef) -> Result<VideoEncodeType> {
        let media = structure.get::<String>("media")?;
        if &media != "video" {
            return Err(anyhow!("Not a video caps"));
        }

        let encoding_name = structure.get::<String>("encoding-name")?;

        let encoding = match encoding_name.to_ascii_uppercase().as_str() {
            "H264" => Ok(VideoEncodeType::H264),
            "H265" => Ok(VideoEncodeType::H265),
            other => Err(anyhow!("Unknown encoding: {other:?}")),
        };

        return encoding;
    }

    while let Some(caps) = rx.recv().await {
        trace!("Received caps: {caps:#?}");

        for structure in caps.iter() {
            match parse_structure(structure).await {
                Ok(video_encode_type) => {
                    trace!("Found encoding {video_encode_type:?} from caps: {caps:?}");

                    return Some(video_encode_type);
                }
                Err(error) => {
                    warn!("Failed getting video encode type from caps: {error:?}");
                }
            }
        }
    }

    None
}

#[instrument(level = "debug", fields(stream_uri = %stream_uri))]
pub async fn get_capture_configuration_from_stream_uri(
    stream_uri: &url::Url,
) -> Result<CaptureConfiguration> {
    let encodes_to_try = get_encode_from_stream_uri(stream_uri)
        .await
        .map(|encode| vec![encode])
        .unwrap_or_else(|error| {
            warn!(
                "Failed getting encode from URI: {error:?}. Trying brute-force with H264 and H265"
            );

            vec![VideoEncodeType::H264, VideoEncodeType::H265]
        });

    for encode in encodes_to_try {
        trace!("Trying the encoding {encode:?}...");

        match get_capture_configuration_using_encoding(stream_uri, encode).await {
            video_capture_configuration @ Ok(_) => return video_capture_configuration,
            Err(error) => {
                trace!("Failed getting capture configuration: {error:?}");
                continue;
            }
        };
    }

    Err(anyhow!("Failed after trying all known encodings"))
}

async fn get_capture_configuration_using_encoding(
    stream_uri: &url::Url,
    encode: VideoEncodeType,
) -> Result<CaptureConfiguration> {
    let mut description = make_source_description_from_stream_uri(stream_uri)?;

    match encode {
        VideoEncodeType::H264 => {
            description.push_str(concat!(
                " ! application/x-rtp, media=(string)video, encoding-name=(string)H264",
                " ! rtph264depay",
                " ! h264parse",
            ));
        }
        VideoEncodeType::H265 => {
            description.push_str(concat!(
                " ! application/x-rtp, media=(string)video, encoding-name=(string)H265",
                " ! rtph265depay",
                " ! h265parse",
            ));
        }
        _unsupported => unreachable!(),
    }

    description.push_str(" ! fakesink name=sink sync=false async=false");

    debug!("Brute-force probe pipeline: {description}");

    let pipeline = gst::parse::launch(&description)
        .context("Failed to create pipeline")?
        .downcast::<gst::Pipeline>()
        .expect("Pipeline is not a valid gst::Pipeline");

    let sink = pipeline
        .by_name("sink")
        .context("description without sink")?;

    let (tx, rx) = mpsc::channel(100);

    let sink_pad = sink.static_pad("sink").context("no sink pad")?;
    let probe_id = setup_pad_and_probe(&sink_pad, tx.clone()).context("Failed to add probe")?;

    if let Err(error) = pipeline.set_state(gst::State::Playing) {
        return Err(anyhow!(
            "Failed setting Pipeline state to Playing. Reason: {error:?}"
        ));
    } else if let Err(error) =
        wait_for_element_state_async(pipeline.downgrade(), ::gst::State::Playing, 100, 5).await
    {
        warn!("Pipeline slow to reach Playing ({error:?}), waiting for caps anyway...");
    }

    let video_capture_configuration = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        wait_for_video_capture_configuration(rx, &encode),
    )
    .await;

    sink_pad.remove_probe(probe_id);

    teardown_probe_pipeline(pipeline).await;

    video_capture_configuration?.context("Not found")
}

/// Tear down a temporary probe pipeline without blocking the async runtime.
///
/// `rtspsrc` can block indefinitely in `set_state(Null)` when the remote
/// RTSP server is unresponsive or the session had no data flowing.  Moving
/// the state change to `spawn_blocking` with a timeout prevents the caller
/// (typically the stream watcher) from stalling.
async fn teardown_probe_pipeline(pipeline: gst::Pipeline) {
    const TEARDOWN_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(5);

    if pipeline.current_state() >= gst::State::Paused {
        pipeline.send_event(gst::event::Eos::new());
    }

    let pipeline_clone = pipeline.clone();
    let result = tokio::time::timeout(
        TEARDOWN_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            if let Err(error) = pipeline_clone.set_state(gst::State::Null) {
                warn!("Probe pipeline set_state(Null) failed: {error:?}");
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!("Probe pipeline teardown task panicked: {error}"),
        Err(_) => {
            warn!(
                "Probe pipeline teardown timed out after {TEARDOWN_TIMEOUT:?}, \
                 leaving cleanup to background thread"
            );
        }
    }
}

fn setup_pad_and_probe(pad: &gst::Pad, tx: mpsc::Sender<gst::Caps>) -> Option<gst::PadProbeId> {
    let probe_id = pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, {
        let tx = tx.clone();

        move |_pad, info| {
            if let Some(gst::PadProbeData::Event(ref ev)) = info.data
                && let gst::EventView::Caps(caps_event) = ev.view() {
                    let caps = caps_event.caps();

                    let _ = tx.try_send(caps.to_owned());
                }
            gst::PadProbeReturn::Ok
        }
    });

    if let Some(caps) = pad.current_caps() {
        let _ = tx.try_send(caps);
    }

    probe_id
}

#[instrument(level = "debug", skip_all)]
async fn wait_for_video_capture_configuration(
    mut rx: mpsc::Receiver<gst::Caps>,
    encode: &VideoEncodeType,
) -> Option<CaptureConfiguration> {
    #[instrument(level = "debug", skip(structure))]
    async fn parse_structure(
        structure: &gst::StructureRef,
        encode: &VideoEncodeType,
    ) -> Result<CaptureConfiguration> {
        let width = structure.get::<i32>("width").context("No width")? as u32;
        let height = structure.get::<i32>("height").context("No height")? as u32;
        let framerate = structure
            .get::<gst::Fraction>("framerate")
            .context("No framerate")?;

        let video_capture_configuration = CaptureConfiguration::Video(VideoCaptureConfiguration {
            encode: encode.clone(),
            height,
            width,
            frame_interval: FrameInterval {
                numerator: framerate.denom() as u32,
                denominator: framerate.numer() as u32,
            },
        });

        return Ok(video_capture_configuration);
    }

    while let Some(caps) = rx.recv().await {
        trace!("Received caps: {caps:#?}");

        for structure in caps.iter() {
            match parse_structure(structure, encode).await {
                Ok(video_capture_configuration) => {
                    trace!(
                        "Found video_capture_configuration {video_capture_configuration:?} from caps: {caps:?}"
                    );

                    return Some(video_capture_configuration);
                }
                Err(error) => {
                    warn!("Failed getting video encode type from caps: {error:?}");
                }
            }
        }
    }

    None
}

/// Set a GObject property by name, logging a warning on failure instead of
/// panicking.  For enum-typed properties the value is resolved via
/// [`gst::glib::EnumClass`]: `i32` values are mapped by numeric value, and
/// `&str` values are looked up by nick.
pub fn try_set_property(element: &gst::Element, name: &str, value: impl Into<gst::glib::Value>) {
    let Some(pspec) = element.find_property(name) else {
        warn!(
            "Property '{name}' not found on element '{}'",
            element.type_()
        );
        return;
    };

    let value = value.into();
    if let Some(enum_class) = gst::glib::EnumClass::with_type(pspec.value_type()) {
        if let Ok(int_val) = value.get::<i32>() {
            if let Some(enum_value) = enum_class.to_value(int_val) {
                element.set_property_from_value(name, &enum_value);
            } else {
                warn!(
                    "Enum value {int_val} is not valid for property '{name}' on element '{}'",
                    element.type_()
                );
            }
            return;
        }
        if let Ok(nick) = value.get::<&str>() {
            match enum_class.to_value_by_nick(nick) {
                Some(enum_value) => element.set_property_from_value(name, &enum_value),
                None => warn!(
                    "Enum nick '{nick}' is not valid for property '{name}' on element '{}'",
                    element.type_()
                ),
            }
            return;
        }
    }

    element.set_property_from_value(name, &value);
}

/// Remove a single passthrough element from its pipeline chain by splicing
/// its upstream and downstream neighbours together, then setting the element
/// to Null and removing it from its parent bin.
///
/// # Safety contract
/// Must be called from a pad-probe callback on a **different** element's pad
/// (not the excised element's own pad). `set_state(Null)` joins the excised
/// element's streaming thread; if that thread is the one executing the probe
/// callback, the call deadlocks.
pub fn excise_single_element(element: &gst::Element) -> Result<()> {
    let name = element.name().to_string();

    let sink_pad = element
        .static_pad("sink")
        .context("element has no sink pad")?;
    let src_pad = element
        .static_pad("src")
        .context("element has no src pad")?;

    let upstream_pad = sink_pad.peer().context("sink pad has no upstream peer")?;
    let downstream_pad = src_pad.peer().context("src pad has no downstream peer")?;

    let upstream_elem = upstream_pad
        .parent_element()
        .map(|e| e.name().to_string())
        .unwrap_or_default();
    let downstream_elem = downstream_pad
        .parent_element()
        .map(|e| e.name().to_string())
        .unwrap_or_default();

    debug!(
        "Excising {name}: unlinking {upstream_elem}:{} → {name} → {downstream_elem}:{}",
        upstream_pad.name(),
        downstream_pad.name(),
    );

    upstream_pad
        .unlink(&sink_pad)
        .map_err(|e| anyhow!("unlink upstream→element: {e}"))?;
    src_pad
        .unlink(&downstream_pad)
        .map_err(|e| anyhow!("unlink element→downstream: {e}"))?;

    upstream_pad
        .link(&downstream_pad)
        .map_err(|e| anyhow!("relink upstream→downstream: {e:?}"))?;

    debug!(
        "Excising {name}: relinked {upstream_elem}:{} → {downstream_elem}:{}",
        upstream_pad.name(),
        downstream_pad.name(),
    );

    let removed = if let Some(bin) = element.parent().and_then(|p| p.downcast::<gst::Bin>().ok()) {
        match bin.remove(element) {
            Ok(_) => true,
            Err(error) => {
                warn!("Excising {name}: remove from bin failed: {error}");
                false
            }
        }
    } else {
        false
    };

    let null_ok = match element.set_state(gst::State::Null) {
        Ok(_) => true,
        Err(error) => {
            warn!("Excising {name}: set_state(Null) failed: {error}");
            false
        }
    };
    if null_ok
        && let Err(error) = wait_for_element_state_sync(element, gst::State::Null, 100, 2) {
            warn!("Excising {name}: {error}");
        }

    debug!("Excising {name}: done (null={null_ok}, removed={removed})");

    Ok(())
}

/// Excises the internal queue that `proxysrc` creates, eliminating one frame of
/// intermediate buffering. Must be called from a pad-probe on the queue's src pad
/// once the first buffer has flowed.
pub fn excise_proxysrc_queue(queue: &glib::WeakRef<gst::Element>) {
    let Some(queue_ref) = queue.upgrade() else {
        return;
    };

    let Some(upstream_pad) = queue_ref
        .static_pad("sink")
        .and_then(|sink_pad| sink_pad.peer())
    else {
        warn!("Failed to find upstream pad for proxysrc queue excision");
        return;
    };

    info!("Proxysrc queue excision: Installing BLOCK_DOWNSTREAM probe");

    let queue = queue.clone();
    upstream_pad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_pad, _info| {
        let Some(queue_ref) = queue.upgrade() else {
            return gst::PadProbeReturn::Remove;
        };

        match excise_single_element(&queue_ref) {
            Ok(()) => info!("Proxysrc queue excision: Queue successfully excised"),
            Err(error) => warn!("Failed to excise proxysrc queue: {error:?}"),
        }

        gst::PadProbeReturn::Remove
    });
}

/// Hooks into the pipeline to excise every `rtpjitterbuffer` that `rtspsrc`'s
/// internal `rtpbin` creates. Without the jitterbuffer, retransmission (NACK)
/// cannot function, so `do-retransmission` is also forced to `false`.
pub fn bypass_jitterbuffer(pipeline: &gst::Pipeline) {
    for element in pipeline
        .iterate_recurse()
        .into_iter()
        .filter_map(Result::ok)
    {
        if element
            .factory()
            .map(|f| f.name() == "rtspsrc")
            .unwrap_or(false)
        {
            element.set_property("do-retransmission", false);
            debug!("Disabled do-retransmission on {}", element.name());
        }
    }

    pipeline.connect("deep-element-added", false, move |values| {
        let element = values[2].get::<gst::Element>().expect("Invalid argument");

        let is_jitterbuffer = element
            .factory()
            .map(|f| f.name() == "rtpjitterbuffer")
            .unwrap_or(false);

        if !is_jitterbuffer {
            return None;
        }

        let name = element.name().to_string();
        let excision_delay_time = std::time::Duration::from_secs(5);
        debug!(
            "Detected {name}, scheduling delayed excision ({}s)",
            excision_delay_time.as_secs()
        );

        let element_weak = element.downgrade();
        let name_clone = name.clone();
        std::thread::spawn(move || {
            std::thread::sleep(excision_delay_time);
            let Some(element) = element_weak.upgrade() else {
                debug!("{name_clone} was dropped before excision timer fired");
                return;
            };
            // Skip excision if the parent pipeline is already tearing down.
            // Racing with set_state(Null) can deadlock GStreamer internals.
            let skip = element
                .parent()
                .and_then(|p| p.downcast::<gst::Element>().ok())
                .is_some_and(|p| p.current_state() < gst::State::Paused);
            if skip {
                debug!("{name_clone}: parent pipeline is below Paused, skipping excision");
                return;
            }
            let Some(upstream_pad) = element.static_pad("sink").and_then(|p| p.peer()) else {
                warn!("{name_clone} has no upstream peer, cannot excise");
                return;
            };
            debug!("Excision timer fired for {name_clone}, adding block probe on upstream pad");
            upstream_pad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |_pad, _info| {
                match excise_single_element(&element) {
                    Ok(()) => debug!("Excised {name_clone} from pipeline"),
                    Err(error) => error!("Failed to excise {name_clone}: {error:#}"),
                }
                gst::PadProbeReturn::Remove
            });
        });

        None
    });
}

const INTERESTING_PROPERTIES: &[&str] = &[
    "leaky",
    "max-size-buffers",
    "max-size-bytes",
    "max-size-time",
    "silent",
    "flush-on-eos",
    "sync",
    "async",
    "latency",
    "do-retransmission",
    "aggregate-mode",
    "config-interval",
    "is-live",
    "udp-buffer-size",
    "location",
    "pt",
    "drop-on-latency",
];

pub fn dump_bin_elements(bin: &gst::Bin, label: &str) {
    use std::fmt::Write;

    let mut out = format!("{label} elements:\n");

    for (i, element) in bin
        .iterate_recurse()
        .into_iter()
        .filter_map(Result::ok)
        .enumerate()
    {
        let factory_name = element
            .factory()
            .map(|f| f.name().to_string())
            .unwrap_or_else(|| "?".into());
        let name = element.name();

        let _ = write!(out, "  [{i}] {factory_name} \"{name}\"");

        for &prop_name in INTERESTING_PROPERTIES {
            if element.find_property(prop_name).is_some() {
                let val = element.property_value(prop_name);
                let display = val
                    .serialize()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| format!("{val:?}"));
                let _ = write!(out, " {prop_name}={display}");
            }
        }
        out.push('\n');

        for pad in element.pads() {
            let dir = match pad.direction() {
                gst::PadDirection::Sink => "sink",
                gst::PadDirection::Src => "src",
                _ => "?",
            };
            let caps_str = pad
                .current_caps()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "(not negotiated)".into());
            let peer = pad
                .peer()
                .map(|p| {
                    let peer_elem = p
                        .parent_element()
                        .map(|e| e.name().to_string())
                        .unwrap_or_default();
                    format!(" -> {peer_elem}:{}", p.name())
                })
                .unwrap_or_default();
            let _ = writeln!(out, "      {dir}:{}{peer}  {caps_str}", pad.name());
        }
    }

    info!("{out}");
}
