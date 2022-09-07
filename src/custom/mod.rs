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
    let default_environment: CustomEnvironment = match cli::manager::default_settings() {
        Some(value) => CustomEnvironment::from_str(value).unwrap(),
        None => {
            return vec![];
        }
    };

    match default_environment {
        CustomEnvironment::BlueROVUDP => bluerov::udp(),
        CustomEnvironment::BlueROVRTSP => bluerov::rtsp(),
    }
}
