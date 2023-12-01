use clap::ValueEnum;

use crate::cli;
use crate::video_stream::types::VideoAndStreamInformation;

mod bluerov;

#[derive(ValueEnum, PartialEq, Debug, Clone)]
#[clap(rename_all = "verbatim")]
pub enum CustomEnvironment {
    BlueROVUDP,
    BlueROVRTSP,
}

pub fn create_default_streams() -> Vec<VideoAndStreamInformation> {
    match cli::manager::default_settings() {
        Some(CustomEnvironment::BlueROVUDP) => bluerov::udp(),
        Some(CustomEnvironment::BlueROVRTSP) => bluerov::rtsp(),
        _ => vec![],
    }
}
