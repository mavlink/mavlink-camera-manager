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

pub fn wait_for_element_state(
    element_weak: gst::glib::WeakRef<gst::Pipeline>,
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

        trials -= 1;
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

        trials -= 1;
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
                "rtspsrc name=source location={stream_uri} is-live=true latency=0 do-retransmission=true protocols=tcp"
            ))
        }
        "udp" => {
            Ok(format!(
                "udpsrc name=source address={address} port={port} close-socket=false auto-multicast=true do-timestamp=true",
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
        " ! fakesink name=sink sync=false",
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
        tokio::time::timeout(tokio::time::Duration::from_secs(15), wait_for_encode(rx)).await;

    sink_pad.remove_probe(probe_id);

    if pipeline.current_state() == gst::State::Playing {
        pipeline.send_event(gst::event::Eos::new());
    }
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        return Err(anyhow!(
            "Failed setting Pipeline state to Null. Reason: {error:?}"
        ));
    } else if let Err(error) =
        wait_for_element_state_async(pipeline.downgrade(), ::gst::State::Null, 100, 30).await
    {
        error!("Failed setting Pipeline state to Null. Reason: {error:?}");
    }

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

    description.push_str(" ! application/x-rtp, media=(string)video");

    match encode {
        VideoEncodeType::H264 => description.push_str(" ! rtph264depay ! h264parse ! avdec_h264"),
        VideoEncodeType::H265 => description.push_str(" ! rtph265depay ! h265parse ! avdec_h265"),
        _unsupported => unreachable!(),
    }

    description.push_str(" ! fakesink name=sink sync=false");

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

    let video_capture_configuration = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        wait_for_video_capture_configuration(rx, &encode),
    )
    .await;

    sink_pad.remove_probe(probe_id);

    if pipeline.current_state() == gst::State::Playing {
        pipeline.send_event(gst::event::Eos::new());
    }
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        return Err(anyhow!(
            "Failed setting Pipeline state to Null. Reason: {error:?}"
        ));
    } else if let Err(error) =
        wait_for_element_state_async(pipeline.downgrade(), ::gst::State::Null, 100, 30).await
    {
        error!("Failed setting Pipeline state to Null. Reason: {error:?}");
    }

    video_capture_configuration?.context("Not found")
}

fn setup_pad_and_probe(pad: &gst::Pad, tx: mpsc::Sender<gst::Caps>) -> Option<gst::PadProbeId> {
    let probe_id = pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, {
        let tx = tx.clone();

        move |_pad, info| {
            if let Some(gst::PadProbeData::Event(ref ev)) = info.data {
                if let gst::EventView::Caps(caps_event) = ev.view() {
                    let caps = caps_event.caps();

                    let _ = tx.try_send(caps.to_owned());
                }
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
