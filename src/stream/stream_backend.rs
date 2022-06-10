use super::types::*;
use super::video_stream_redirect::VideoStreamRedirect;
use super::video_stream_rtsp::VideoStreamRtsp;
use super::video_stream_tcp::VideoStreamTcp;
use super::video_stream_udp::VideoStreamUdp;
use super::video_stream_webrtc::VideoStreamWebRTC;
use super::webrtc::utils::{is_webrtcsink_available, webrtc_usage_hint};
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video_stream::types::VideoAndStreamInformation;
use simple_error::SimpleError;

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
    check_encode_support(video_and_stream_information)?;
    check_scheme_and_encoding_compatibility(video_and_stream_information)?;
    return create_stream(video_and_stream_information);
}

fn check_endpoints(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints.is_empty() {
        return Err(SimpleError::new("Endpoints are empty".to_string()));
    }

    let endpoints_have_different_schemes = endpoints
        .windows(2)
        .any(|win| win[0].scheme() != win[1].scheme());

    let is_custom_webrtc = endpoints.iter().any(|endpoint| {
        endpoint.scheme() == "stun" || endpoint.scheme() == "turn" || endpoint.scheme() == "ws"
    });

    // We only allow different schemes for custom WebRTC
    if endpoints_have_different_schemes && !is_custom_webrtc {
        return Err(SimpleError::new(format!(
            "Endpoints scheme are not the same: {endpoints:#?}",
        )));
    }

    return Ok(());
}

fn check_encode_support(
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
            return Err(SimpleError::new(format!(
                "Encode is not supported and also unknown: {name}",
            )))
        }
        VideoEncodeType::H264
        | VideoEncodeType::H265
        | VideoEncodeType::YUYV
        | VideoEncodeType::MJPG => (),
        _ => {
            return Err(SimpleError::new(format!(
                "Only H264, H265, YUYV and MJPG encodes are supported now, used: {encode:?}",
            )));
        }
    };

    return Ok(());
}

fn check_scheme_and_encoding_compatibility(
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
            "udp" | "udp265"| "rtsp" | "mpegts" | "tcpmpeg" => scheme.to_string(),
            "tcp" => return Err(SimpleError::new(format!("Endpoints with the \"tcp\" scheme are not supported by Mavlink, REDIRECT is meant to advertise an already existing stream using Mavlink protocol, but Mavlink protocol doesn't specify any TCP with RTP. If you meant to use TPC with MPEG, you should use the perhaps you meant \"tcpmpeg\" scheme. Encode: {encode:?}, Endpoints: {endpoints:#?}"))),
            _ => return Err(SimpleError::new(format!(
                "The URL's scheme for REDIRECT endpoints should be \"udp\", \"udp265\", \"rtsp\", \"mpegts\" \"tcpmpeg\", but was: {scheme:?}",
            )))
        };
    } else {
        match scheme {
            "udp" | "tcp" | "rtsp" | "webrtc" | "stun" | "turn" | "ws" => (), // No encoding restrictions for these schemes.
            "udp265" => {
                if VideoEncodeType::H265 != encode {
                    return Err(SimpleError::new(format!("Endpoint with \"udp265\" scheme only supports H265 encode. Encode: {encode:?}, Endpoints: {endpoints:#?}")));
                }
            }
            "mpegts" | "tcpmpeg" | _ => {
                return Err(SimpleError::new(format!(
                    "Scheme is not accepted as stream endpoint: {scheme}",
                )));
            }
        }
    }

    return Ok(());
}

fn check_for_multiple_endpoints(endpoints: &Vec<url::Url>) -> Result<(), SimpleError> {
    if endpoints.len() > 1 {
        let scheme = endpoints[0].scheme().to_uppercase();
        return Err(SimpleError::new(format!(
            "Multiple {scheme} endpoints are not acceptable: {endpoints:#?}",
        )));
    }
    Ok(())
}

fn check_for_host_and_port(endpoints: &Vec<url::Url>) -> Result<(), SimpleError> {
    let no_host_or_port = endpoints
        .iter()
        .any(|endpoint| endpoint.host().is_none() || endpoint.port().is_none());

    if no_host_or_port {
        let scheme = endpoints[0].scheme().to_uppercase();
        return Err(SimpleError::new(format!(
            "Endpoint with {scheme} scheme should contain host and port. Endpoints: {endpoints:#?}",
        )));
    }
    Ok(())
}

fn create_udp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    check_for_host_and_port(endpoints)?;

    Ok(StreamType::UDP(VideoStreamUdp::new(
        video_and_stream_information,
    )?))
}

fn create_rtsp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    check_for_multiple_endpoints(endpoints)?;

    let endpoint = &endpoints[0];
    if endpoint.scheme() != "rtsp" {
        return Err(SimpleError::new(format!(
            "The URL's scheme for RTSP endpoints should be \"rtsp\", but was: {:?}",
            endpoint.scheme()
        )));
    }
    if endpoint.host_str() != "0.0.0.0".into() {
        return Err(SimpleError::new(format!(
            "The URL's host for RTSP endpoints should be \"0.0.0.0\", but was: {:?}",
            endpoint.host_str()
        )));
    }
    if endpoint.port() != Some(8554) {
        return Err(SimpleError::new(format!(
            "The URL's port for RTSP endpoints should be \"8554\", but was: {:?}",
            endpoint.port()
        )));
    }
    if endpoint.path_segments().iter().count() != 1 {
        return Err(SimpleError::new(format!(
            "The URL's path for RTSP endpoints should have only one segment (e.g.: \"segmentA\" and not \"segmentA/segmentB\"), but was: {:?}",
            endpoint.path()
        )));
    }

    Ok(StreamType::RTSP(VideoStreamRtsp::new(
        video_and_stream_information,
        endpoint.path().to_string(),
    )?))
}

fn create_tcp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;

    check_for_host_and_port(endpoints)?;
    check_for_multiple_endpoints(endpoints)?;

    Ok(StreamType::TCP(VideoStreamTcp::new(
        video_and_stream_information,
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
        return Err(SimpleError::new(format!(
                "WebRTC stream cannot be created because the gstreamer webrtcsink plugin is not available. {}",
                crate::stream::webrtc::utils::webrtcsink_installation_instructions()
        )));
    }

    let usage_hint = webrtc_usage_hint();

    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if check_for_host_and_port(
        &endpoints
            .clone()
            .into_iter()
            .filter(|endpoint| endpoint.scheme() != "webrtc")
            .collect(),
    )
    .is_err()
    {
        return Err(SimpleError::new(format!(
            "Endpoint with 'stun://', 'turn://' and 'ws://' schemes should have a host and port. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }

    for scheme in vec!["webrtc", "stun", "turn", "ws"] {
        if endpoints
            .iter()
            .filter(|endpoint| endpoint.scheme() == scheme)
            .count()
            > 1
        {
            return Err(SimpleError::new(format!(
                "More than one 'webrtc://' {scheme} was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
            )));
        }
    }

    if endpoints
        .iter()
        .any(|endpoint| endpoint.scheme() == "webrtc")
        && endpoints.len() > 1
    {
        return Err(SimpleError::new(format!(
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
            "tcp" => create_tcp_stream(video_and_stream_information),
            "rtsp" => create_rtsp_stream(video_and_stream_information),
            "webrtc" | "stun" | "turn" | "ws" => {
                create_webrtc_turn_stream(video_and_stream_information)
            }
            something => Err(SimpleError::new(format!("Unsupported scheme: {something}"))),
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
                typ: VideoSourceLocalType::Unknown("TestPotatoCam".into()),
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
