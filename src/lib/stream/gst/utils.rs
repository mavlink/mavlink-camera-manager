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

#[allow(dead_code)] // TODO: Use this to check all used plugins are available
pub fn is_gst_plugin_available(plugin_name: &str, min_version: Option<&str>) -> bool {
    // reference: https://github.com/GStreamer/gst/blob/b4ca58df7624b005a33e182a511904d7cceea890/tools/gst-inspect.c#L2148

    if let Err(error) = gst::init() {
        tracing::error!("Error! {error}");
    }

    if let Some(min_version) = min_version {
        let version = semver::Version::parse(min_version).unwrap();
        gst::Registry::get().check_feature_version(
            plugin_name,
            version.major.try_into().unwrap(),
            version.minor.try_into().unwrap(),
            version.patch.try_into().unwrap(),
        )
    } else {
        gst::Registry::get().lookup_feature(plugin_name).is_some()
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

#[instrument(level = "debug")]
pub async fn get_encode_from_stream_uri(stream_uri: &url::Url) -> Option<VideoEncodeType> {
    use gst::prelude::*;

    let description = match stream_uri.scheme() {
        "rtsp" => {
            format!(
                concat!(
                    "rtspsrc location={location} is-live=true latency=0",
                    " ! typefind name=typefinder minimum=1",
                    " ! fakesink name=fakesink sync=false"
                ),
                location = stream_uri.to_string(),
            )
        }
        "udp" => {
            format!(
                concat!(
                    "udpsrc address={address} port={port} close-socket=false auto-multicast=true do-timestamp=true",
                    " ! typefind name=typefinder minimum=1",
                    " ! fakesink name=fakesink sync=false"
                ),
                address = stream_uri.host()?,
                port = stream_uri.port()?,
            )
        }
        unsupported => {
            warn!("Scheme {unsupported:#?} is not supported for Redirect Pipelines");
            return None;
        }
    };

    let pipeline = gst::parse::launch(&description)
        .expect("Failed to create pipeline")
        .downcast::<gst::Pipeline>()
        .expect("Pipeline is not a valid gst::Pipeline");

    let typefinder = pipeline.by_name("typefinder")?;

    let (tx, rx) = mpsc::channel(10);

    typefinder.connect("have-type", false, move |values| {
        let _typefinder = values[0].get::<gst::Element>().expect("Invalid argument");
        let _probability = values[1].get::<u32>().expect("Invalid argument");
        let caps = values[2].get::<gst::Caps>().expect("Invalid argument");

        if let Err(error) = tx.blocking_send(caps) {
            error!("Failed sending caps from typefinder: {error:?}");
        }

        None
    });

    pipeline
        .set_state(gst::State::Playing)
        .expect("Failed to set pipeline to Playing");

    let encode = tokio::time::timeout(tokio::time::Duration::from_secs(15), wait_for_encode(rx))
        .await
        .ok()
        .flatten();

    pipeline
        .set_state(gst::State::Null)
        .expect("Failed to set pipeline to Null");

    encode
}

async fn wait_for_encode(mut rx: mpsc::Receiver<gst::Caps>) -> Option<VideoEncodeType> {
    if let Some(caps) = rx.recv().await {
        let structure = caps.structure(0)?;

        let encoding_name = structure.get::<String>("encoding-name").ok()?;

        let encoding = match encoding_name.to_ascii_uppercase().as_str() {
            "H264" => Some(VideoEncodeType::H264),
            "H265" => Some(VideoEncodeType::H265),
            _unsupported => None,
        };

        trace!("Found encoding {encoding:?} from caps: {caps:?}");

        return encoding;
    }

    None
}

#[instrument(level = "debug")]
pub async fn get_capture_configuration_from_stream_uri(
    stream_uri: &url::Url,
) -> Result<CaptureConfiguration> {
    let description = match stream_uri.scheme() {
        "rtsp" => {
            format!(
                concat!("rtspsrc location={location} is-live=true latency=0",),
                location = stream_uri.to_string(),
            )
        }
        "udp" => {
            format!(
                concat!(
                    "udpsrc address={address} port={port} close-socket=false auto-multicast=true do-timestamp=true",
                ),
                address = stream_uri.host().context("URI without host")?,
                port = stream_uri.port().context("URI without port")?,
            )
        }
        unsupported => {
            return Err(anyhow!(
                "Scheme {unsupported:#?} is not supported for Redirect Pipelines"
            ));
        }
    };

    let known_encodings = [VideoEncodeType::H264, VideoEncodeType::H265];
    for encode in known_encodings {
        debug!("Trying the encoding {encode:?}...");

        match get_capture_configuration_using_encoding(description.clone(), encode).await {
            video_capture_configuration @ Ok(_) => return video_capture_configuration,
            Err(error) => {
                debug!("Failed getting capture configuration: {error:?}");
                continue;
            }
        };
    }

    Err(anyhow!("Failed after trying all known encodings"))
}

async fn get_capture_configuration_using_encoding(
    mut description: String,
    encode: VideoEncodeType,
) -> Result<CaptureConfiguration> {
    use gst::prelude::*;

    description.push_str(" ! application/x-rtp ");

    match encode {
        VideoEncodeType::H264 => description.push_str(" ! rtph264depay ! avdec_h264"),
        VideoEncodeType::H265 => description.push_str(" ! rtph265depay ! avdec_h265"),
        _unsupported => unreachable!(),
    }

    description.push_str(concat!(
        " ! typefind name=typefinder minimum=99",
        " ! fakesink name=fakesink sync=false",
    ));

    let pipeline = gst::parse::launch(&description)
        .expect("Failed to create pipeline")
        .downcast::<gst::Pipeline>()
        .expect("Pipeline is not a valid gst::Pipeline");

    let typefinder = pipeline
        .by_name("typefinder")
        .context("Failed to get typefinder")?;

    let (tx, rx) = mpsc::channel(10);

    typefinder.connect("have-type", false, move |values| {
        let _typefinder = values[0].get::<gst::Element>().expect("Invalid argument");
        let _probability = values[1].get::<u32>().expect("Invalid argument");
        let caps = values[2].get::<gst::Caps>().expect("Invalid argument");

        if let Err(error) = tx.blocking_send(caps) {
            error!("Failed sending caps from typefinder: {error:?}");
        }

        None
    });

    pipeline
        .set_state(gst::State::Playing)
        .expect("Failed to set pipeline to Playing");

    let video_capture_configuration = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        wait_for_video_capture_configuration(rx, encode),
    )
    .await
    .ok()
    .context("Timeout");

    pipeline
        .set_state(gst::State::Null)
        .expect("Failed to set pipeline to Null");

    video_capture_configuration?
}

async fn wait_for_video_capture_configuration(
    mut rx: mpsc::Receiver<gst::Caps>,
    encode: VideoEncodeType,
) -> Result<CaptureConfiguration> {
    let Some(caps) = rx.recv().await else {
        return Err(anyhow!("No caps received"));
    };

    debug!("Received caps: {caps:#?}");

    let structure = caps.structure(0).context("Empty structure")?;

    let width = structure.get::<i32>("width").context("No width")? as u32;
    let height = structure.get::<i32>("height").context("No height")? as u32;
    let framerate = structure
        .get::<gst::Fraction>("framerate")
        .context("No framerate")?;

    let frame_interval = FrameInterval {
        numerator: framerate.denom() as u32,
        denominator: framerate.numer() as u32,
    };

    let video_capture_configuration = CaptureConfiguration::Video(VideoCaptureConfiguration {
        encode,
        height,
        width,
        frame_interval,
    });

    trace!("Found video_capture_configuration {video_capture_configuration:?} from caps: {caps:?}");

    Ok(video_capture_configuration)
}
