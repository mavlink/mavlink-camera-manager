use url::Url;

use crate::stream::types::*;
use crate::video::{self, types::*, video_source::VideoSourceAvailable};
use crate::video_stream::types::VideoAndStreamInformation;

pub fn udp() -> Vec<VideoAndStreamInformation> {
    video::video_source_local::VideoSourceLocal::cameras_available()
        .iter()
        .filter(|cam| {
            cam.inner()
                .formats()
                .iter()
                .any(|format| format.encode == VideoEncodeType::H264)
        })
        .enumerate()
        .map(|(index, cam)| {
            let formats = cam.inner().formats();
            let format = formats
                .iter()
                .find(|format| format.encode == VideoEncodeType::H264)
                .unwrap();
            let size = format.sizes.first().unwrap();
            VideoAndStreamInformation {
                name: format!("UDP Stream {}", index),
                stream_information: StreamInformation {
                    endpoints: vec![
                        Url::parse(&format!("udp://192.168.2.1:{}", 5600 + index)).unwrap()
                    ],
                    configuration: CaptureConfiguration {
                        encode: format.encode.clone(),
                        height: size.height,
                        width: size.width,
                        frame_interval: size.intervals.first().unwrap().clone(),
                    },
                },
                video_source: cam.clone(),
            }
        })
        .collect()
}
