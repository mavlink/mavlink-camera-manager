use std::str::FromStr;

use clap::arg_enum;

use crate::cli;
use crate::video_stream::types::VideoAndStreamInformation;

mod bluerov;

arg_enum! {
    #[derive(PartialEq, Debug)]
    pub enum CustomEnvironment {
        BlueROVUDP,
        BlueROVRTSP,
    }
}

pub fn create_default_streams() -> Vec<VideoAndStreamInformation> {
    match cli::manager::default_settings().map(CustomEnvironment::from_str) {
        Some(Ok(CustomEnvironment::BlueROVUDP)) => bluerov::udp(),
        Some(Ok(CustomEnvironment::BlueROVRTSP)) => bluerov::rtsp(),
        _ => vec![],
    }
}
