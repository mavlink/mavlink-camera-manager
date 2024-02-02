use url::Url;

use crate::stream::types::*;
use crate::video::{self, types::*, video_source_gst::VideoSourceGstType};
use crate::video_stream::types::VideoAndStreamInformation;

pub fn take_webrtc_stream() -> Vec<VideoAndStreamInformation> {
    let size = STANDARD_SIZES.last().unwrap();
    vec![VideoAndStreamInformation {
        name: format!("WebRTC fake stream for thread leak"),
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
            extended_configuration: None,
        },
        video_source: VideoSourceType::Gst(video::video_source_gst::VideoSourceGst {
            name: "Fake".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }]
}
