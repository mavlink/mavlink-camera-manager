use url::Url;

use crate::network::utils::get_visible_qgc_address;
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

            // Get the biggest resolution possible
            let mut sizes = format.sizes.clone();
            sizes.sort_by(|first_size, second_size| {
                (10 * first_size.width + first_size.height)
                    .cmp(&(10 * second_size.width + second_size.height))
            });
            let size = sizes.last().unwrap();

            VideoAndStreamInformation {
                name: format!("UDP Stream {}", index),
                stream_information: StreamInformation {
                    endpoints: vec![
                        Url::parse(&format!("udp://192.168.2.1:{}", 5600 + index)).unwrap()
                    ],
                    configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                        encode: format.encode.clone(),
                        height: size.height,
                        width: size.width,
                        frame_interval: size.intervals.first().unwrap().clone(),
                    }),
                    extended_configuration: None,
                },
                video_source: cam.clone(),
            }
        })
        .collect()
}

pub fn rtsp() -> Vec<VideoAndStreamInformation> {
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

            // Get the biggest resolution possible
            let mut sizes = format.sizes.clone();
            sizes.sort_by(|first_size, second_size| {
                (10 * first_size.width + first_size.height)
                    .cmp(&(10 * second_size.width + second_size.height))
            });
            let size = sizes.last().unwrap();

            let visible_qgc_ip_address = get_visible_qgc_address();

            VideoAndStreamInformation {
                name: format!("RTSP Stream {index}"),
                stream_information: StreamInformation {
                    endpoints: vec![Url::parse(&format!(
                        "rtsp://{visible_qgc_ip_address}:8554/video_{index}"
                    ))
                    .unwrap()],
                    configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                        encode: format.encode.clone(),
                        height: size.height,
                        width: size.width,
                        frame_interval: size.intervals.first().unwrap().clone(),
                    }),
                    extended_configuration: None,
                },
                video_source: cam.clone(),
            }
        })
        .collect()
}
