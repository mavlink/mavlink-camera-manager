use anyhow::{anyhow, Result};
use gst::prelude::*;
use tracing::*;

use crate::video::types::VideoEncodeType;

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
pub async fn get_encode_from_rtspsrc(stream_uri: &url::Url) -> Option<VideoEncodeType> {
    use gst::prelude::*;

    let description = format!(
        concat!(
            "rtspsrc location={location} is-live=true latency=0",
            " ! fakesink name=fakesink sync=false"
        ),
        location = stream_uri.to_string(),
    );

    let pipeline = gst::parse::launch(&description)
        .expect("Failed to create pipeline")
        .downcast::<gst::Pipeline>()
        .expect("Pipeline is not a valid gst::Pipeline");

    pipeline
        .set_state(gst::State::Playing)
        .expect("Failed to set pipeline to Playing");

    let fakesink = pipeline
        .by_name("fakesink")
        .expect("Fakesink not found in pipeline");
    let pad = fakesink.static_pad("sink").expect("Sink pad not found");

    let encode = tokio::time::timeout(tokio::time::Duration::from_secs(5), wait_for_encode(pad))
        .await
        .ok()
        .flatten();

    pipeline
        .set_state(gst::State::Null)
        .expect("Failed to set pipeline to Null");

    encode
}

pub async fn wait_for_encode(pad: gst::Pad) -> Option<VideoEncodeType> {
    loop {
        if let Some(caps) = pad.current_caps() {
            trace!("caps from rtspsrc: {caps:?}");

            if let Some(structure) = caps.structure(0) {
                if let Ok(encoding_name) = structure.get::<String>("encoding-name") {
                    let encoding = match encoding_name.to_ascii_uppercase().as_str() {
                        "H264" => Some(VideoEncodeType::H264),
                        "H265" => Some(VideoEncodeType::H265),
                        _unsupported => None,
                    };

                    break encoding;
                }
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}
