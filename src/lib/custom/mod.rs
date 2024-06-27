use clap::ValueEnum;

use crate::cli;
use crate::video_stream::types::VideoAndStreamInformation;

mod bluerov;
mod test;

#[derive(ValueEnum, PartialEq, Debug, Clone)]
#[clap(rename_all = "verbatim")]
pub enum CustomEnvironment {
    BlueROVUDP,
    BlueROVRTSP,
    WebRTCTest,
}

pub fn create_default_streams() -> Vec<VideoAndStreamInformation> {
    match cli::manager::default_settings() {
        Some(CustomEnvironment::BlueROVUDP) => bluerov::udp(),
        Some(CustomEnvironment::BlueROVRTSP) => bluerov::rtsp(),
        Some(CustomEnvironment::WebRTCTest) => test::take_webrtc_stream(),
        _ => vec![],
    }
}
