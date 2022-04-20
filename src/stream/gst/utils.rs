pub fn is_gstreamer_plugin_available(plugin_name: &str, min_version: &str) -> bool {
    // reference: https://github.com/GStreamer/gstreamer/blob/b4ca58df7624b005a33e182a511904d7cceea890/tools/gst-inspect.c#L2148

    if let Err(error) = gstreamer::init() {
        log::error!("Error! {error}");
    }

    let version = semver::Version::parse(min_version).unwrap();
    return gstreamer::Registry::get().check_feature_version(
        plugin_name,
        version.major.try_into().unwrap(),
        version.minor.try_into().unwrap(),
        version.patch.try_into().unwrap(),
    );
}
