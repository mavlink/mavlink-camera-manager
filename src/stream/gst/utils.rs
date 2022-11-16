use gstreamer::prelude::*;
use simple_error::{simple_error, SimpleResult};

#[derive(Debug)]
pub struct PluginRankConfig {
    pub name: String,
    pub rank: gstreamer::Rank,
}

#[allow(dead_code)] // TODO: Use this to check all used plugins are available
pub fn is_gstreamer_plugin_available(plugin_name: &str, min_version: &str) -> bool {
    // reference: https://github.com/GStreamer/gstreamer/blob/b4ca58df7624b005a33e182a511904d7cceea890/tools/gst-inspect.c#L2148

    if let Err(error) = gstreamer::init() {
        tracing::error!("Error! {error}");
    }

    let version = semver::Version::parse(min_version).unwrap();
    return gstreamer::Registry::get().check_feature_version(
        plugin_name,
        version.major.try_into().unwrap(),
        version.minor.try_into().unwrap(),
        version.patch.try_into().unwrap(),
    );
}

pub fn set_plugin_rank(plugin_name: &str, rank: gstreamer::Rank) -> SimpleResult<()> {
    if let Err(error) = gstreamer::init() {
        tracing::error!("Error! {error}");
    }

    if let Some(feature) = gstreamer::Registry::get().lookup_feature(plugin_name) {
        feature.set_rank(rank);
    } else {
        return Err(simple_error!(format!(
            "Cannot found Gstreamer feature {plugin_name:#?} in the registry."
        )));
    }

    Ok(())
}
