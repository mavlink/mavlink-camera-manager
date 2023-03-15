use url::Url;

use crate::stream::types::*;
use crate::video::{self, types::*, video_source::VideoSourceAvailable};
use crate::video_stream::types::VideoAndStreamInformation;
use log::*;

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
        .flat_map(|(index, cam)| {
            let formats = cam.inner().formats();
            let format = formats
                .iter()
                .find(|format| format.encode == VideoEncodeType::H264)
                .unwrap();

            // Get the biggest resolution possible
            let mut sizes = format.sizes.clone();
            sizes.sort_by(|first_size, second_size| {
                (10 * first_size.width + first_size.height)
                    .cmp(&(10 * second_size.width + second_size.height))
            });

            let Some(size) = sizes.last() else {
                warn!("Unable to find a valid size for {:?}", cam);
                return None;
            };

            Some(VideoAndStreamInformation {
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
                    extended_configuration: None,
                },
                video_source: cam.clone(),
            })
        })
        .collect()
}
