use crate::stream::gst::utils::is_gstreamer_plugin_available;

const WEBRTCSINK_PLUGIN_NAME: &str = "webrtcsink";
const WEBRTCSINK_VERSION: &str = "0.1.0";
const WEBRTCSINK_REPOSITORY: &str = "https://github.com/centricular/webrtcsink";

pub fn is_webrtcsink_available() -> bool {
    is_gstreamer_plugin_available(WEBRTCSINK_PLUGIN_NAME, WEBRTCSINK_VERSION)
}

pub fn webrtcsink_installation_instructions() -> String {
    format!(
        concat!(
            "Be sure to have the webrtcsink gstreamer plugin installed on your system's gst plugin path, ",
            "or add the webrtcsink plugin path location to the GST_PLUGIN_PATH environment variable before ",
            "starting mavlink-camera-manager. This plugin is available at: {:?}, and the required version is {:?}."
        ),
        WEBRTCSINK_REPOSITORY,
        WEBRTCSINK_VERSION
    )
}

pub fn webrtc_usage_hint() -> String {
    concat!(
        "To use the default local servers, pass just one 'webrtc://'. ",
        "Alternatively, custom servers can be used in place of the default local ones ",
        "by passing a comma-separated list with up to one of each: ",
        " 'stun://<ip>:<port>' for the STUN server, and/or",
        " 'turn://[<user>:<password>@]<ip>:<port>' for the TURN server, and/or",
        " 'wp://<ip>:<port>' for the SIGNALLING server using the webrtcsink's protocol."
    )
    .to_string()
}
