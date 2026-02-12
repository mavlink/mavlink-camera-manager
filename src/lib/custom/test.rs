use url::Url;

use mcm_api::v1::{
    stream::{
        CaptureConfiguration, StreamInformation, VideoAndStreamInformation,
        VideoCaptureConfiguration,
    },
    video::{FrameInterval, VideoEncodeType, VideoSourceGst, VideoSourceGstType, VideoSourceType},
};

use crate::video::types::STANDARD_SIZES;

pub fn take_webrtc_stream() -> Vec<VideoAndStreamInformation> {
    let size = STANDARD_SIZES.last().unwrap();
    vec![VideoAndStreamInformation {
        name: "WebRTC fake stream for thread leak".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse("udp://0.0.0.0:8554/test").unwrap()],
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode: VideoEncodeType::H264,
                height: size.1,
                width: size.0,
                frame_interval: FrameInterval {
                    denominator: 10,
                    numerator: 1,
                },
            }),
            extended_configuration: Some(ExtendedConfiguration {
                disable_lazy: true,
                ..Default::default()
            }),
        },
        video_source: VideoSourceType::Gst(VideoSourceGst {
            name: "Fake".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }]
}
