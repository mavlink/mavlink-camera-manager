mod bluerov;
mod test;

use clap::ValueEnum;

use crate::{cli, video_stream::types::VideoAndStreamInformation};

#[derive(ValueEnum, PartialEq, Debug, Clone)]
#[clap(rename_all = "verbatim")]
pub enum CustomEnvironment {
    BlueROVUDP,
    BlueROVRTSP,
    WebRTCTest,
}

pub async fn create_default_streams() -> Vec<VideoAndStreamInformation> {
    match cli::manager::default_settings() {
        Some(CustomEnvironment::BlueROVUDP) => bluerov::udp().await,
        Some(CustomEnvironment::BlueROVRTSP) => bluerov::rtsp().await,
        Some(CustomEnvironment::WebRTCTest) => test::take_webrtc_stream(),
        _ => vec![],
    }
}
