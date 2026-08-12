mod bluerov;
#[cfg(feature = "webrtc-test")]
mod test;

use clap::ValueEnum;

use crate::{cli, video_stream::types::VideoAndStreamInformation};

#[derive(ValueEnum, PartialEq, Debug, Clone)]
#[clap(rename_all = "verbatim")]
pub enum CustomEnvironment {
    BlueROVUDP,
    BlueROVRTSP,
    #[cfg(feature = "webrtc-test")]
    WebRTCTest,
}

pub async fn create_default_streams() -> Vec<VideoAndStreamInformation> {
    let Some(environment) = cli::manager::default_settings() else {
        return vec![];
    };

    // Device providers can still be settling when settings init builds defaults on a fresh
    // boot; retry briefly so BlueROVUDP/RTSP is not persisted as an empty stream list.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let streams = match environment {
            CustomEnvironment::BlueROVUDP => bluerov::udp().await,
            CustomEnvironment::BlueROVRTSP => bluerov::rtsp().await,
            #[cfg(feature = "webrtc-test")]
            CustomEnvironment::WebRTCTest => test::take_webrtc_stream(),
        };
        if !streams.is_empty() || tokio::time::Instant::now() >= deadline {
            return streams;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
