use super::types::*;
use super::video_stream_redirect::VideoStreamRedirect;
use super::video_stream_rtsp::VideoStreamRtsp;
use super::video_stream_udp::VideoStreamUdp;
use super::video_stream_webrtc::VideoStreamWebRTC;
use super::webrtc::utils::{is_webrtcsink_available, webrtc_usage_hint};
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video_stream::types::VideoAndStreamInformation;
use simple_error::{simple_error, SimpleError};

pub trait StreamBackend {
    fn start(&mut self) -> bool;
    fn stop(&mut self) -> bool;
    fn is_running(&self) -> bool;
    fn restart(&mut self);
    fn pipeline(&self) -> String;
    fn allow_same_endpoints(&self) -> bool;
}

pub fn new(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    check_endpoints(video_and_stream_information)?;
    check_encode(video_and_stream_information)?;
    check_scheme(video_and_stream_information)?;
    return create_stream(video_and_stream_information);
}

fn check_endpoints(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints.is_empty() {
        return Err(simple_error!("Endpoints are empty"));
    }

    let endpoints_have_different_schemes = endpoints
        .windows(2)
        .any(|win| win[0].scheme() != win[1].scheme());

    let is_custom_webrtc = endpoints.iter().any(|endpoint| {
        endpoint.scheme() == "stun" || endpoint.scheme() == "turn" || endpoint.scheme() == "ws"
    });

    // We only allow different schemes for custom WebRTC
    if endpoints_have_different_schemes && !is_custom_webrtc {
        return Err(simple_error!(format!(
            "Endpoints scheme are not the same: {endpoints:#?}"
        )));
    }

    return Ok(());
}

fn check_encode(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    let encode = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::VIDEO(configuration) => configuration.encode.clone(),
        CaptureConfiguration::REDIRECT(_) => return Ok(()),
    };

    match &encode {
        VideoEncodeType::UNKNOWN(name) => {
            return Err(simple_error!(format!(
                "Encode is not supported and also unknown: {name}",
            )))
        }
        VideoEncodeType::H264 | VideoEncodeType::YUYV | VideoEncodeType::MJPG => (),
        _ => {
            return Err(simple_error!(format!(
                "Only H264, YUYV and MJPG encodes are supported now, used: {encode:?}",
            )));
        }
    };

    return Ok(());
}

fn check_scheme(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;
    let encode = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::VIDEO(configuration) => configuration.encode.clone(),
        CaptureConfiguration::REDIRECT(_) => VideoEncodeType::UNKNOWN("".into()),
    };
    let scheme = endpoints.first().unwrap().scheme();

    if let VideoSourceType::Redirect(_) = video_and_stream_information.video_source {
        match scheme {
            "udp" | "udp265"| "rtsp" | "mpegts" | "tcp" => scheme.to_string(),
            _ => return Err(simple_error!(format!(
                "The URL's scheme for REDIRECT endpoints should be \"udp\", \"udp265\", \"rtsp\", \"mpegts\" or \"tcp\", but was: {scheme:?}",
            )))
        };
    } else {
        match scheme {
            "rtsp" => {
                if endpoints.len() > 1 {
                    return Err(simple_error!(format!(
                        "Multiple RTSP endpoints are not acceptable: {endpoints:#?}"
                    )));
                }
            }
            "udp" => {
                if VideoEncodeType::H265 == encode {
                    return Err(simple_error!("Endpoint with udp scheme only supports H264, encode type is H265, the scheme should be udp265."));
                }

                //UDP endpoints should contain both host and port
                let no_host_or_port = endpoints
                    .iter()
                    .any(|endpoint| endpoint.host().is_none() || endpoint.port().is_none());

                if no_host_or_port {
                    return Err(simple_error!(format!(
                        "Endpoint with udp scheme should contain host and port. Endpoints: {endpoints:#?}"
                    )));
                }
            }
            "udp265" => {
                if VideoEncodeType::H265 != encode {
                    return Err(simple_error!(format!("Endpoint with udp265 scheme only supports H265 encode. Encode: {encode:?}, Endpoints: {endpoints:#?}")));
                }
            }
            "webrtc" | "stun" | "turn" | "ws" => {
                let incomplete_endpoint = endpoints.iter().any(|endpoint| {
                    (endpoint.scheme() != "webrtc")
                        && (endpoint.host().is_none() || endpoint.port().is_none())
                });

                if incomplete_endpoint {
                    return Err(simple_error!(format!(
                        "Endpoint with 'stun://', 'turn://' and 'ws://' schemes should have a host and port, like \"stun://0.0.0.0:3478\". Endpoints: {endpoints:#?}",
                    )));
                }
            }
            _ => {
                return Err(simple_error!(format!(
                    "Scheme is not accepted as stream endpoint: {scheme}",
                )));
            }
        }
    }

    return Ok(());
}

fn create_udp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    Ok(StreamType::UDP(VideoStreamUdp::new(
        video_and_stream_information,
    )?))
}

fn create_rtsp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;
    let endpoint = &endpoints[0];
    if endpoint.scheme() != "rtsp" {
        return Err(simple_error!(format!(
            "The URL's scheme for RTSP endpoints should be \"rtsp\", but was: {:?}",
            endpoint.scheme()
        )));
    }
    if endpoint.port() != Some(8554) {
        return Err(simple_error!(format!(
            "The URL's port for RTSP endpoints should be \"8554\", but was: {:?}",
            endpoint.port()
        )));
    }
    if endpoint.path_segments().iter().count() != 1 {
        return Err(simple_error!(format!(
            "The URL's path for RTSP endpoints must have one segment (e.g.: \"segmentA\" and not \"segmentA/segmentB\"), but was: {:?}",
            endpoint.path()
        )));
    }

    Ok(StreamType::RTSP(VideoStreamRtsp::new(
        video_and_stream_information,
        endpoint.path().to_string(),
    )?))
}

fn create_redirect_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let endpoint = &video_and_stream_information.stream_information.endpoints[0];

    Ok(StreamType::REDIRECT(VideoStreamRedirect::new(
        endpoint.scheme().to_string(),
    )?))
}

fn create_webrtc_turn_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    if !is_webrtcsink_available() {
        return Err(simple_error!(format!(
                "WebRTC stream cannot be created because the gstreamer webrtcsink plugin is not available. {}",
                crate::stream::webrtc::utils::webrtcsink_installation_instructions()
        )));
    }

    let usage_hint = webrtc_usage_hint();

    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "webrtc")
        .count()
        > 1
    {
        return Err(simple_error!(format!(
            "More than one 'webrtc://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "stun")
        .count()
        > 1
    {
        return Err(simple_error!(format!(
            "More than one 'stun://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "turn")
        .count()
        > 1
    {
        return Err(simple_error!(format!(
            "More than one 'turn://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "ws")
        .count()
        > 1
    {
        return Err(simple_error!(format!(
            "More than one 'ws://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .any(|endpoint| endpoint.scheme() == "webrtc")
        && endpoints.len() > 1
    {
        return Err(simple_error!(format!(
            "'stun://', 'turn://' or 'ws://' schemes cannot be passed along with a 'webrtc://' scheme. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }

    Ok(StreamType::WEBRTC(VideoStreamWebRTC::new(
        video_and_stream_information,
    )?))
}

fn create_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    // The scheme was validated by "new" function
    if let VideoSourceType::Redirect(_) = video_and_stream_information.video_source {
        create_redirect_stream(video_and_stream_information)
    } else {
        let endpoint = &video_and_stream_information
            .stream_information
            .endpoints
            .iter()
            .next()
            .unwrap();
        match endpoint.scheme() {
            "udp" => create_udp_stream(video_and_stream_information),
            "rtsp" => create_rtsp_stream(video_and_stream_information),
            "webrtc" | "stun" | "turn" | "ws" => {
                create_webrtc_turn_stream(video_and_stream_information)
            }
            something => Err(simple_error!(format!("Unsupported scheme: {something}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::types::CaptureConfiguration;
    use crate::video::{
        types::FrameInterval,
        video_source_local::{VideoSourceLocal, VideoSourceLocalType},
    };

    use url::Url;

    fn stream_type_fabricator(
        stream_endpoints: &Vec<Url>,
        video_encode_type: &VideoEncodeType,
    ) -> StreamType {
        let stream = create_stream(&VideoAndStreamInformation {
            name: "Test".into(),
            stream_information: StreamInformation {
                endpoints: stream_endpoints.clone(),
                configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                    encode: video_encode_type.clone(),
                    height: 720,
                    width: 1280,
                    frame_interval: FrameInterval {
                        numerator: 1,
                        denominator: 30,
                    },
                }),
                extended_configuration: None,
            },
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: "PotatoCam".into(),
                device_path: "/dev/video42".into(),
                typ: VideoSourceLocalType::Usb("TestPotatoCam".into()),
            }),
        });

        assert!(stream.is_ok());
        stream.unwrap()
    }

    #[test]
    fn test_udp() {
        let pipeline_testing = vec![
            (VideoEncodeType::H264, "v4l2src device=/dev/video42 ! video/x-h264,width=1280,height=720,framerate=30/1 ! h264parse ! queue ! rtph264pay name=pay0 config-interval=10 pt=96 ! multiudpsink clients=192.168.0.1:42"),
            (VideoEncodeType::YUYV, "v4l2src device=/dev/video42 ! video/x-raw,format=YUY2,width=1280,height=720,framerate=30/1 ! videoconvert ! video/x-raw,format=UYVY ! rtpvrawpay name=pay0 ! application/x-rtp,payload=96,sampling=YCbCr-4:2:2 ! multiudpsink clients=192.168.0.1:42"),
            (VideoEncodeType::MJPG, "v4l2src device=/dev/video42 ! image/jpeg,width=1280,height=720,framerate=30/1 ! rtpjpegpay name=pay0 pt=96 ! multiudpsink clients=192.168.0.1:42"),
        ];

        for (encode_type, expected_pipeline) in pipeline_testing.iter() {
            let stream = stream_type_fabricator(
                &vec![Url::parse("udp://192.168.0.1:42").unwrap()],
                encode_type,
            );
            let pipeline = match &stream {
                StreamType::UDP(video_stream_udp) => video_stream_udp.pipeline(),
                _any_other_stream_type => panic!("Failed to create UDP stream: {stream:?}."),
            };
            assert_eq!(&pipeline, expected_pipeline);
        }
    }

    #[test]
    fn test_rtsp() {
        let pipeline_testing = vec![
            (VideoEncodeType::H264, "v4l2src device=/dev/video42 ! video/x-h264,width=1280,height=720,framerate=30/1 ! h264parse ! queue ! rtph264pay name=pay0 config-interval=10 pt=96"),
            (VideoEncodeType::YUYV, "v4l2src device=/dev/video42 ! video/x-raw,format=YUY2,width=1280,height=720,framerate=30/1 ! videoconvert ! video/x-raw,format=UYVY ! rtpvrawpay name=pay0 ! application/x-rtp,payload=96,sampling=YCbCr-4:2:2"),
            (VideoEncodeType::MJPG, "v4l2src device=/dev/video42 ! image/jpeg,width=1280,height=720,framerate=30/1 ! rtpjpegpay name=pay0 pt=96"),
        ];

        for (encode_type, expected_pipeline) in pipeline_testing.iter() {
            let stream = stream_type_fabricator(
                &vec![Url::parse("rtsp://0.0.0.0:8554/test").unwrap()],
                encode_type,
            );
            let pipeline = match &stream {
                StreamType::RTSP(video_stream_rtsp) => video_stream_rtsp.pipeline(),
                _any_other_stream_type => panic!("Failed to create RTSP stream: {stream:?}."),
            };
            assert_eq!(&pipeline, expected_pipeline);
        }
    }

    fn get_webrtc_test_pipeline(default_endpoints: bool) -> Vec<(VideoEncodeType, String)> {
        let (stun, turn, signaller) = match default_endpoints {
            true => (
                "stun://0.0.0.0:3478",
                "turn://user:pwd@0.0.0.0:3478",
                "ws://0.0.0.0:6021/",
            ),
            false => (
                "stun://stun.l.google.com:19302",
                "turn://test:1qaz2wsx@turn.homeneural.net:3478",
                "ws://192.168.3.4:44019/",
            ),
        };

        vec![
            (
                VideoEncodeType::H264,
                format!(
                    "v4l2src device=/dev/video42 \
                    ! video/x-h264,width=1280,height=720,framerate=30/1 \
                    ! decodebin3 \
                    ! videoconvert \
                    ! webrtcsink \
                    stun-server={stun} \
                    turn-server={turn} \
                    signaller::address={signaller} \
                    video-caps=video/x-h264 \
                    display-name=\"Test\" \
                    congestion-control=0 \
                    do-retransmission=false \
                    do-fec=false \
                    enable-data_channel_navigation=false"
                ),
            ),
            (
                VideoEncodeType::YUYV,
                format!(
                    "v4l2src device=/dev/video42 \
                    ! video/x-raw,format=YUY2,width=1280,height=720,framerate=30/1 \
                    ! decodebin3 \
                    ! videoconvert \
                    ! webrtcsink \
                    stun-server={stun} \
                    turn-server={turn} \
                    signaller::address={signaller} \
                    video-caps=video/x-h264 \
                    display-name=\"Test\" \
                    congestion-control=0 \
                    do-retransmission=false \
                    do-fec=false \
                    enable-data_channel_navigation=false"
                ),
            ),
            (
                VideoEncodeType::MJPG,
                format!(
                    "v4l2src device=/dev/video42 \
                    ! image/jpeg,width=1280,height=720,framerate=30/1 \
                    ! decodebin3 \
                    ! videoconvert \
                    ! webrtcsink \
                    stun-server={stun} \
                    turn-server={turn} \
                    signaller::address={signaller} \
                    video-caps=video/x-h264 \
                    display-name=\"Test\" \
                    congestion-control=0 \
                    do-retransmission=false \
                    do-fec=false \
                    enable-data_channel_navigation=false"
                ),
            ),
        ]
    }

    #[test]
    fn test_webrtc_default_servers() {
        let pipeline_testing = get_webrtc_test_pipeline(true);
        for (encode_type, expected_pipeline) in pipeline_testing.iter() {
            let stream =
                stream_type_fabricator(&vec![Url::parse("webrtc://").unwrap()], encode_type);
            let pipeline = match &stream {
                StreamType::WEBRTC(video_stream_webrtc) => video_stream_webrtc.pipeline(),
                _any_other_stream_type => panic!("Failed to create WebRTC stream: {stream:?}."),
            };
            assert_eq!(&pipeline, expected_pipeline);
        }
    }

    #[test]
    fn test_webrtc_custom_servers() {
        let pipeline_testing = get_webrtc_test_pipeline(false);

        for (encode_type, expected_pipeline) in pipeline_testing.iter() {
            let stream = stream_type_fabricator(
                &vec![
                    Url::parse("stun://stun.l.google.com:19302").unwrap(),
                    Url::parse("turn://test:1qaz2wsx@turn.homeneural.net:3478").unwrap(),
                    Url::parse("ws://192.168.3.4:44019").unwrap(),
                ],
                encode_type,
            );
            let pipeline = match &stream {
                StreamType::WEBRTC(video_stream_webrtc) => video_stream_webrtc.pipeline(),
                _any_other_stream_type => panic!("Failed to create WebRTC stream: {stream:?}."),
            };
            assert_eq!(&pipeline, expected_pipeline);
        }
    }
}
