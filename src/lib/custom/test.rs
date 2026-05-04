use url::Url;

use crate::{
    stream::types::*,
    video::{self, types::*, video_source_gst::VideoSourceGstType},
    video_stream::types::VideoAndStreamInformation,
};

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
        video_source: VideoSourceType::Gst(video::video_source_gst::VideoSourceGst {
            name: "Fake".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }]
}

/// Create a fake H264 stream info for testing file sink recording
/// Uses videotestsrc (ball pattern) with H264 encoding
pub fn create_fake_h264_stream_info() -> VideoAndStreamInformation {
    VideoAndStreamInformation {
        name: "Test H264 Stream".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse("udp://0.0.0.0:5600").unwrap()],
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode: VideoEncodeType::H264,
                height: 720,
                width: 1280,
                frame_interval: FrameInterval {
                    denominator: 30,
                    numerator: 1,
                },
            }),
            extended_configuration: None,
        },
        video_source: VideoSourceType::Gst(video::video_source_gst::VideoSourceGst {
            name: "Fake H264".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }
}

/// Create a fake H265 stream info for testing file sink recording
/// Uses videotestsrc (ball pattern) with H265 encoding
pub fn create_fake_h265_stream_info() -> VideoAndStreamInformation {
    VideoAndStreamInformation {
        name: "Test H265 Stream".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse("udp://0.0.0.0:5601").unwrap()],
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode: VideoEncodeType::H265,
                height: 720,
                width: 1280,
                frame_interval: FrameInterval {
                    denominator: 30,
                    numerator: 1,
                },
            }),
            extended_configuration: None,
        },
        video_source: VideoSourceType::Gst(video::video_source_gst::VideoSourceGst {
            name: "Fake H265".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }
}

/// Create a temporary directory for recording tests
/// Returns the path to the temp directory
pub fn temp_recording_path() -> std::path::PathBuf {
    let temp_dir = std::env::temp_dir().join(format!(
        "mavlink-camera-manager-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&temp_dir).expect("Failed to create temp directory");
    temp_dir
}

/// Clean up a temporary recording directory
pub fn cleanup_recording_path(path: &std::path::Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_dir_all(path) {
            eprintln!(
                "Warning: Failed to cleanup temp directory {:?}: {}",
                path, e
            );
        }
    }
}

/// Create a VideoAndStreamInformation with an unsupported encoding (for testing error cases)
pub fn create_unsupported_encoding_stream_info() -> VideoAndStreamInformation {
    VideoAndStreamInformation {
        name: "Unsupported Encoding Stream".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse("udp://0.0.0.0:5602").unwrap()],
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode: VideoEncodeType::Yuyv, // YUYV is not supported for file recording
                height: 720,
                width: 1280,
                frame_interval: FrameInterval {
                    denominator: 30,
                    numerator: 1,
                },
            }),
            extended_configuration: None,
        },
        video_source: VideoSourceType::Gst(video::video_source_gst::VideoSourceGst {
            name: "Fake YUYV".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }
}
