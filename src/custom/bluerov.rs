use url::Url;

use crate::network::utils::get_visible_qgc_address;
use crate::stream::types::*;
use crate::video::{self, types::*, video_source::VideoSourceAvailable};
use crate::video_stream::types::VideoAndStreamInformation;
use tracing::*;

fn get_cameras_with_encode_type(encode: VideoEncodeType) -> Vec<VideoSourceType> {
    let cameras = video::video_source_local::VideoSourceLocal::cameras_available();
    cameras
        .iter()
        .filter(move |cam| {
            cam.inner()
                .formats()
                .iter()
                .any(|format| format.encode == encode)
        })
        .cloned()
        .collect()
}

fn sort_sizes(sizes: &mut [Size]) {
    sizes.sort_by(|first_size, second_size| {
        (10 * first_size.width + first_size.height)
            .cmp(&(10 * second_size.width + second_size.height))
    });
}

pub fn udp() -> Vec<VideoAndStreamInformation> {
    get_cameras_with_encode_type(VideoEncodeType::H264)
        .iter()
        .enumerate()
        .filter_map(|(index, cam)| {
            let formats = cam.inner().formats();
            let Some(format) = formats
                .iter()
                .find(|format| format.encode == VideoEncodeType::H264) else {
                    warn!("Unable to find a valid format for {cam:?}");
                    return None;
                };

            // Get the biggest resolution possible
            let mut sizes = format.sizes.clone();
            sort_sizes(&mut sizes);

            let Some(size) = sizes.last() else {
                warn!("Unable to find a valid size for {cam:?}");
                return None;
            };

            Some(VideoAndStreamInformation {
                name: format!("UDP Stream {}", index),
                stream_information: StreamInformation {
                    endpoints: vec![
                        Url::parse(&format!("udp://192.168.2.1:{}", 5600 + index)).ok()?
                    ],
                    configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                        encode: format.encode.clone(),
                        height: size.height,
                        width: size.width,
                        frame_interval: size.intervals.first()?.clone(),
                    }),
                    extended_configuration: None,
                },
                video_source: cam.clone(),
            })
        })
        .collect()
}

pub fn rtsp() -> Vec<VideoAndStreamInformation> {
    get_cameras_with_encode_type(VideoEncodeType::H264)
        .iter()
        .enumerate()
        .filter_map(|(index, cam)| {
            let formats = cam.inner().formats();
            let Some(format) = formats
                .iter()
                .find(|format| format.encode == VideoEncodeType::H264) else {
                    warn!("Unable to find a valid format for {cam:?}");
                    return None;
                };

            // Get the biggest resolution possible
            let mut sizes = format.sizes.clone();
            sort_sizes(&mut sizes);

            let Some(size) = sizes.last() else {
                warn!("Unable to find a valid size for {cam:?}");
                return None;
            };

            let visible_qgc_ip_address = get_visible_qgc_address();

            Some(VideoAndStreamInformation {
                name: format!("RTSP Stream {index}"),
                stream_information: StreamInformation {
                    endpoints: vec![Url::parse(&format!(
                        "rtsp://{visible_qgc_ip_address}:8554/video_{index}"
                    ))
                    .ok()?],
                    configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                        encode: format.encode.clone(),
                        height: size.height,
                        width: size.width,
                        frame_interval: size.intervals.first()?.clone(),
                    }),
                    extended_configuration: None,
                },
                video_source: cam.clone(),
            })
        })
        .collect()
}
