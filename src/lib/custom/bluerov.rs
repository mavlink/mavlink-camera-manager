use tracing::*;
use url::Url;

use crate::{
    network::utils::get_visible_qgc_address,
    stream::types::*,
    video::{
        self,
        types::*,
        video_source::{VideoSourceAvailable, VideoSourceFormats},
    },
    video_stream::types::VideoAndStreamInformation,
};

async fn get_cameras_with_encode_type(encode: VideoEncodeType) -> Vec<VideoSourceType> {
    let mut result = Vec::new();

    let cameras = video::video_source_local::VideoSourceLocal::cameras_available().await;

    for camera in cameras.iter() {
        if camera
            .formats()
            .await
            .iter()
            .any(|format| format.encode == encode)
        {
            result.push(camera.clone());
        }
    }

    result
}

fn sort_sizes(sizes: &mut [Size]) {
    sizes.sort_by(|first_size, second_size| {
        (10 * first_size.width + first_size.height)
            .cmp(&(10 * second_size.width + second_size.height))
    });
}

pub async fn udp() -> Vec<VideoAndStreamInformation> {
    let mut result = Vec::new();

    let sources = get_cameras_with_encode_type(VideoEncodeType::H264).await;

    for (index, source) in sources.iter().enumerate() {
        let formats = source.formats().await;

        let Some(format) = formats
            .iter()
            .find(|format| format.encode == VideoEncodeType::H264)
        else {
            warn!("Unable to find a valid format for {source:?}");
            continue;
        };

        // Get the biggest resolution possible
        let mut sizes = format.sizes.clone();
        sort_sizes(&mut sizes);
        let Some(size) = sizes.last() else {
            warn!("Unable to find a valid size for {source:?}");
            continue;
        };

        let Some(frame_interval) = size.intervals.first().cloned() else {
            warn!("Unable to find a frame interval");
            continue;
        };

        let endpoint = match Url::parse(&format!("udp://192.168.2.1:{}", 5600 + index)) {
            Ok(url) => url,
            Err(error) => {
                warn!("Failed to parse URL: {error:?}");
                continue;
            }
        };

        result.push(VideoAndStreamInformation {
            name: format!("UDP Stream {index}"),
            stream_information: StreamInformation {
                endpoints: vec![endpoint],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: format.encode.clone(),
                    height: size.height,
                    width: size.width,
                    frame_interval,
                }),
                extended_configuration: None,
            },
            video_source: source.clone(),
        })
    }

    result
}

pub async fn rtsp() -> Vec<VideoAndStreamInformation> {
    let mut result = Vec::new();

    let sources = get_cameras_with_encode_type(VideoEncodeType::H264).await;

    for (index, source) in sources.iter().enumerate() {
        let formats = source.formats().await;

        let Some(format) = formats
            .iter()
            .find(|format| format.encode == VideoEncodeType::H264)
        else {
            warn!("Unable to find a valid format for {source:?}");
            continue;
        };

        // Get the biggest resolution possible
        let mut sizes = format.sizes.clone();
        sort_sizes(&mut sizes);
        let Some(size) = sizes.last() else {
            warn!("Unable to find a valid size for {source:?}");
            continue;
        };

        let Some(frame_interval) = size.intervals.first().cloned() else {
            warn!("Unable to find a frame interval");
            continue;
        };

        let visible_qgc_ip_address = get_visible_qgc_address();
        let endpoint = match Url::parse(&format!(
            "rtsp://{visible_qgc_ip_address}:8554/video_{index}"
        )) {
            Ok(url) => url,
            Err(error) => {
                warn!("Failed to parse URL: {error:?}");
                continue;
            }
        };

        result.push(VideoAndStreamInformation {
            name: format!("RTSP Stream {index}"),
            stream_information: StreamInformation {
                endpoints: vec![endpoint],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode: format.encode.clone(),
                    height: size.height,
                    width: size.width,
                    frame_interval,
                }),
                extended_configuration: None,
            },
            video_source: source.clone(),
        });
    }

    result
}
