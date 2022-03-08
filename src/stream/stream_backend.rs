use super::types::*;
use super::video_stream_redirect::VideoStreamRedirect;
use super::video_stream_rtsp::VideoStreamRtsp;
use super::video_stream_udp::VideoStreamUdp;
use crate::stream::signalling_server::DEFAULT_SIGNALLING_ENDPOINT;
use crate::stream::turn_server::{DEFAULT_STUN_ENDPOINT, DEFAULT_TURN_ENDPOINT};
use crate::stream::video_stream_webrtc::VideoStreamWebRTC;
use crate::video::{
    types::{VideoEncodeType, VideoSourceType},
    video_source_gst::VideoSourceGstType,
};
use crate::video_stream::types::VideoAndStreamInformation;
use log::*;
use simple_error::SimpleError;

pub trait StreamBackend {
    fn start(&mut self) -> bool;
    fn stop(&mut self) -> bool;
    fn is_running(&self) -> bool;
    fn restart(&mut self);
    fn set_pipeline_description(&mut self, description: &str);
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
            return Err(SimpleError::new(format!(
                "Encode is not supported and also unknown: {name}",
            )))
        }
        VideoEncodeType::H264 => (),
        _ => {
            return Err(SimpleError::new(format!(
                "Only H264 encode is supported now, used: {:?}",
                encode
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
            _ => return Err(SimpleError::new(format!(
                "The URL's scheme for REDIRECT endpoints should be \"udp\", \"udp265\", \"rtsp\", \"mpegts\" or \"tcp\", but was: {:?}",
                scheme
            )))
        };
    } else {
        match scheme {
            "rtsp" => {
                if endpoints.len() > 1 {
                    return Err(SimpleError::new(format!(
                        "Multiple RTSP endpoints are not acceptable: {:#?}",
                        endpoints
                    )));
                }
            }
            "udp" => {
                if VideoEncodeType::H264 != encode {
                    return Err(SimpleError::new(format!("Endpoint with udp scheme only supports H264 encode. Encode: {:?}, Endpoints: {:#?}", encode, endpoints)));
                }

                if VideoEncodeType::H265 == encode {
                    return Err(SimpleError::new("Endpoint with udp scheme only supports H264, encode type is H265, the scheme should be udp265.".to_string()));
                }

                //UDP endpoints should contain both host and port
                let no_host_or_port = endpoints
                    .iter()
                    .any(|endpoint| endpoint.host().is_none() || endpoint.port().is_none());

                if no_host_or_port {
                    return Err(SimpleError::new(format!(
                        "Endpoint with udp scheme should contain host and port. Endpoints: {:#?}",
                        endpoints
                    )));
                }
            }
            "udp265" => {
                if VideoEncodeType::H265 != encode {
                    return Err(SimpleError::new(format!("Endpoint with udp265 scheme only supports H265 encode. Encode: {:?}, Endpoints: {:#?}", encode, endpoints)));
                }
            }
            "webrtc" | "stun" | "turn" | "ws" => {
                if VideoEncodeType::H264 != encode {
                    return Err(SimpleError::new(format!("Endpoint with 'webrtc://', 'stun://', 'turn://' or 'ws://' schemes only supports H264 encode. Encode: {encode:?}, Endpoints: {endpoints:#?}")));
                }

                let incomplete_endpoint = endpoints.iter().any(|endpoint| {
                    (endpoint.scheme() != "webrtc")
                        && (endpoint.host().is_none() || endpoint.port().is_none())
                });

                if incomplete_endpoint {
                    return Err(SimpleError::new(format!(
                        "Endpoint with 'stun://', 'turn://' and 'ws://' schemes should have a host and port, like \"stun://0.0.0.0:3478\". Endpoints: {endpoints:#?}",
                    )));
                }
            }
            _ => {
                return Err(SimpleError::new(format!(
                    "Scheme is not accepted as stream endpoint: {}",
                    scheme
                )));
            }
        }
    }

    return Ok(());
}

fn create_udp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let configuration = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::VIDEO(configuration) => configuration,
        _ => return Err(SimpleError::new("Unsupported CaptureConfiguration.")),
    };
    let encode = configuration.encode.clone();
    let endpoints = &video_and_stream_information.stream_information.endpoints;
    let video_source = &video_and_stream_information.video_source;

    let video_format = match video_source {
        VideoSourceType::Local(local_device) => {
            if VideoEncodeType::H264 == encode {
                format!(
                    concat!(
                        "v4l2src device={device}",
                        " ! video/x-h264,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                    ),
                    device = &local_device.device_path,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                )
            } else {
                return Err(SimpleError::new(format!(
                    "Unsupported encode for UDP endpoint: {:?}",
                    encode
                )));
            }
        }
        VideoSourceType::Gst(gst_source) => match &gst_source.source {
            VideoSourceGstType::Fake(pattern) => {
                format!(
                        concat!(
                            "videotestsrc pattern={pattern}",
                            " ! video/x-raw,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                            " ! videoconvert",
                            " ! x264enc bitrate=5000",
                            " ! video/x-h264, profile=baseline",
                        ),
                        pattern = pattern,
                        width = configuration.width,
                        height = configuration.height,
                        interval_denominator = configuration.frame_interval.denominator,
                        interval_numerator = configuration.frame_interval.numerator,
                    )
            }
            _ => {
                return Err(SimpleError::new(format!(
                    "Unsupported GST source for UDP endpoint: {:#?}",
                    gst_source
                )));
            }
        },
        something => {
            return Err(SimpleError::new(format!(
                "Unsupported VideoSourceType stream creation: {:#?}",
                something
            )));
        }
    };

    if VideoEncodeType::H264 == encode {
        let udp_encode = concat!(
            " ! h264parse",
            " ! queue",
            " ! rtph264pay config-interval=10 pt=96",
        );

        let clients: Vec<String> = endpoints
            .iter()
            .map(|endpoint| format!("{}:{}", endpoint.host().unwrap(), endpoint.port().unwrap()))
            .collect();
        let clients = clients.join(",");

        let udp_sink = format!(" ! multiudpsink clients={}", clients);

        let pipeline = [&video_format, udp_encode, &udp_sink].join("");
        info!("Created pipeline: {}", pipeline);
        let mut stream = VideoStreamUdp::default();
        stream.set_pipeline_description(&pipeline);
        return Ok(StreamType::UDP(stream));
    }

    return Err(SimpleError::new(format!(
        "Unsupported encode: {:?}",
        encode
    )));
}

fn create_rtsp_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let configuration = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::VIDEO(configuration) => configuration,
        configuration @ _ => {
            return Err(SimpleError::new(format!(
                "Unsupported configuration: {configuration:#?}."
            )))
        }
    };
    let encode = configuration.encode.clone();
    let endpoint = &video_and_stream_information.stream_information.endpoints[0];
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

    let video_source = &video_and_stream_information.video_source;

    let video_format = match video_source {
        VideoSourceType::Local(local_device) => {
            if VideoEncodeType::H264 == encode {
                format!(
                    concat!(
                        "v4l2src device={device}",
                        " ! video/x-h264,width={width},height={height},framerate={interval_denominator}/{interval_numerator},type=video",
                    ),
                    device = &local_device.device_path,
                    width = configuration.width,
                    height = configuration.height,
                    interval_denominator = configuration.frame_interval.denominator,
                    interval_numerator = configuration.frame_interval.numerator,
                )
            } else {
                return Err(SimpleError::new(format!(
                    "Unsupported encode for RTSP endpoint: {:?}",
                    encode
                )));
            }
        }

        VideoSourceType::Gst(gst_source) => match &gst_source.source {
            VideoSourceGstType::Fake(pattern) => {
                format!(
                        concat!(
                            "videotestsrc pattern={pattern}",
                            " ! video/x-raw,width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                            " ! videoconvert",
                            " ! x264enc bitrate=5000",
                            " ! video/x-h264, profile=baseline",
                        ),
                        pattern = pattern,
                        width = configuration.width,
                        height = configuration.height,
                        interval_denominator = configuration.frame_interval.denominator,
                        interval_numerator = configuration.frame_interval.numerator,
                    )
            }
            _ => {
                return Err(SimpleError::new(format!(
                    "Unsupported GST source for RTSP endpoint: {:#?}",
                    gst_source
                )));
            }
        },
        something => {
            return Err(SimpleError::new(format!(
                "Unsupported VideoSourceType stream creation: {:#?}",
                something
            )));
        }
    };

    if VideoEncodeType::H264 == encode {
        let rtsp_encode = concat!(
            " ! h264parse",
            " ! queue",
            " ! rtph264pay name=pay0 config-interval=10 pt=96",
        );

        let pipeline = [&video_format, rtsp_encode].join("");
        info!("Created pipeline: {}", &pipeline);
        let mut stream = VideoStreamRtsp::default();
        stream.set_endpoint_path(&endpoint.path());
        stream.set_pipeline_description(&pipeline);
        return Ok(StreamType::RTSP(stream));
    }

    return Err(SimpleError::new(format!(
        "Unsupported encode: {:?}",
        encode
    )));
}

fn create_redirect_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let endpoint = &video_and_stream_information.stream_information.endpoints[0];
    let mut stream = VideoStreamRedirect::default();
    stream.scheme = endpoint.scheme().to_string();
    return Ok(StreamType::REDIRECT(stream));
}

fn create_webrtc_turn_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let usage_hint = concat!(
        "To use the default local servers, pass just one 'webrtc://'. ",
        "Alternatively, custom servers can be used in place of the default local ones ",
        "by passing a comma-separated list with up to one of each: ",
        " 'stun://<ip>:<port>' for the STUN server, and/or",
        " 'turn://[<user>:<password>@]<ip>:<port>' for the TURN server, and/or",
        " 'wp://<ip>:<port>' for the SIGNALLING server using the webrtcsink's protocol."
    );

    let endpoints = &video_and_stream_information.stream_information.endpoints;

    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "webrtc")
        .count()
        > 1
    {
        return Err(SimpleError::new(format!(
            "More than one 'webrtc://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "stun")
        .count()
        > 1
    {
        return Err(SimpleError::new(format!(
            "More than one 'stun://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "turn")
        .count()
        > 1
    {
        return Err(SimpleError::new(format!(
            "More than one 'turn://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.scheme() == "ws")
        .count()
        > 1
    {
        return Err(SimpleError::new(format!(
            "More than one 'ws://' scheme was passed. {usage_hint}. The endpoints passed were: {endpoints:#?}",
        )));
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

    let mut stun_endpoint = url::Url::parse(DEFAULT_STUN_ENDPOINT).unwrap();
    let mut turn_endpoint = url::Url::parse(DEFAULT_TURN_ENDPOINT).unwrap();
    let mut signalling_endpoint = url::Url::parse(DEFAULT_SIGNALLING_ENDPOINT).unwrap();

    for endpoint in endpoints.iter() {
        match endpoint.scheme() {
            "webrtc" => (),
            "stun" => stun_endpoint = endpoint.to_owned(),
            "turn" => turn_endpoint = endpoint.to_owned(),
            "ws" => signalling_endpoint = endpoint.to_owned(),
            _ => {
                return Err(SimpleError::new(format!(
                    "Only 'webrtc://', 'stun://', 'turn://' and 'ws://' schemes are accepted. {usage_hint}. The scheme passed was: {:#?}\"",
                    endpoint.scheme()
                )))
            }
        }
    }
    debug!("Using the following endpoint for the STUN Server: \"{stun_endpoint}\"");
    debug!("Using the following endpoint for the TURN Server: \"{turn_endpoint}\"",);
    debug!("Using the following endpoint for the Signalling Server: \"{signalling_endpoint}\"",);

    let configuration = match &video_and_stream_information
        .stream_information
        .configuration
    {
        CaptureConfiguration::VIDEO(configuration) => configuration,
        _ => return Err(SimpleError::new(format!(
            "The only accepted configuration type is the VideoCaptureConfiguration, which is like {supported_configuration}, but was {given_configuration:#?} ",
            supported_configuration="VideoCaptureConfiguration { \"encode\": <string>, \"height\": <integer>, \"width\": <integer>, \"frame_interval\": <integer/integer> }",
            given_configuration=video_and_stream_information.stream_information.configuration,
        ))),
    };
    let video_format = format!(
        "width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
        width = configuration.width,
        height = configuration.height,
        interval_denominator = configuration.frame_interval.denominator,
        interval_numerator = configuration.frame_interval.numerator,
    );

    let encode = configuration.encode.clone();
    let video_source = &video_and_stream_information.video_source;
    let pipeline = match video_source {
        VideoSourceType::Local(local_device) => {
            if VideoEncodeType::H264 == encode {
                format!(
                    concat!(
                        "v4l2src device={device}",
                        " ! video/x-h264,{video_format}",
                        " ! decodebin3",
                        " ! videoconvert",
                        " ! webrtcsink stun-server={stun_server} turn-server={turn_server} signaller::address={signaller_server}",
                        " video-caps=video/x-h264,{video_format}",
                    ),
                    device = &local_device.device_path,
                    video_format = video_format,
                    stun_server = stun_endpoint,
                    turn_server = turn_endpoint,
                    signaller_server = signalling_endpoint,
                )
            } else {
                return Err(SimpleError::new(format!(
                    "Unsupported encode for WebRTC endpoint: {encode:?}"
                )));
            }
        }
        VideoSourceType::Gst(gst_source) => match &gst_source.source {
            VideoSourceGstType::Fake(pattern) => {
                format!(
                        concat!(
                            "videotestsrc pattern={pattern}",
                            " ! video/x-raw,{video_format}",
                            " ! videoconvert",
                            " ! webrtcsink stun-server={stun_server} turn-server={turn_server} signaller::address={signaller_server}",
                            " video-caps=video/x-h264,{video_format}",
                        ),
                        pattern = pattern,
                        video_format = video_format,
                        stun_server = stun_endpoint,
                        turn_server = turn_endpoint,
                        signaller_server = signalling_endpoint,
                    )
            }
            _ => {
                return Err(SimpleError::new(format!(
                    "Unsupported GST source for WebRTC endpoint: {gst_source:#?}"
                )));
            }
        },
        something => {
            return Err(SimpleError::new(format!(
                "Unsupported VideoSourceType stream creation: {something:#?}"
            )));
        }
    };

    if VideoEncodeType::H264 != encode {
        return Err(SimpleError::new(format!("Unsupported encode: {encode:?}",)));
    }

    info!("Created pipeline: {pipeline}");
    let mut stream = VideoStreamWebRTC::default();
    stream.set_pipeline_description(&pipeline);
    return Ok(StreamType::WEBRTC(stream));
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

    #[test]
    fn test_udp() {
        let result = create_stream(&VideoAndStreamInformation {
            name: "Test".into(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("udp://192.168.0.1:42").unwrap()],
                configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                    encode: VideoEncodeType::H264,
                    height: 720,
                    width: 1080,
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

        assert!(result.is_ok());
        let result = &result.unwrap();

        let result = match result {
            StreamType::UDP(video_stream_udp) => video_stream_udp,
            _any_other_stream_type => panic!("Failed to create UDP stream: {:?}.", result),
        };
        assert_eq!(result.pipeline(), "v4l2src device=/dev/video42 ! video/x-h264,width=1080,height=720,framerate=30/1 ! h264parse ! queue ! rtph264pay config-interval=10 pt=96 ! multiudpsink clients=192.168.0.1:42");
    }

    #[test]
    fn test_rtsp() {
        let result = create_stream(&VideoAndStreamInformation {
            name: "Test".into(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("rtsp://0.0.0.0:8554/test").unwrap()],
                configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                    encode: VideoEncodeType::H264,
                    height: 720,
                    width: 1080,
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

        assert!(result.is_ok());
        let result = &result.unwrap();

        let result = match result {
            StreamType::RTSP(video_stream_rtsp) => video_stream_rtsp,
            _any_other_stream_type => panic!("Failed to create RTSP stream: {:?}.", result),
        };
        assert_eq!(result.pipeline(), "v4l2src device=/dev/video42 ! video/x-h264,width=1080,height=720,framerate=30/1,type=video ! h264parse ! queue ! rtph264pay name=pay0 config-interval=10 pt=96");
    }

    #[test]
    fn test_webrtc_default_servers() {
        let result = create_stream(&VideoAndStreamInformation {
            name: "Test".into(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("webrtc://").unwrap()],
                configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                    encode: VideoEncodeType::H264,
                    height: 720,
                    width: 1080,
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

        assert!(result.is_ok());
        let result = &result.unwrap();

        let result = match result {
            StreamType::WEBRTC(video_stream_webrtc) => video_stream_webrtc,
            _any_other_stream_type => panic!("Failed to create WebRTC stream: {:?}.", result),
        };
        assert_eq!(
            result.pipeline(),
            concat!(
                "v4l2src device=/dev/video42",
                " ! video/x-h264,width=1080,height=720,framerate=30/1",
                " ! decodebin3",
                " ! videoconvert",
                " ! webrtcsink",
                " stun-server=stun://0.0.0.0:3478",
                " turn-server=turn://user:pwd@0.0.0.0:3478",
                " signaller::address=ws://0.0.0.0:6021/",
                " video-caps=video/x-h264,width=1080,height=720,framerate=30/1",
            )
        );
    }

    #[test]
    fn test_webrtc_custom_servers() {
        let result = create_stream(&VideoAndStreamInformation {
            name: "Test".into(),
            stream_information: StreamInformation {
                endpoints: vec![
                    Url::parse("stun://stun.l.google.com:19302").unwrap(),
                    Url::parse("turn://test:1qaz2wsx@turn.homeneural.net:3478").unwrap(),
                    Url::parse("ws://192.168.3.4:44019").unwrap(),
                ],
                configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                    encode: VideoEncodeType::H264,
                    height: 720,
                    width: 1080,
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

        assert!(result.is_ok());
        let result = &result.unwrap();

        let result = match result {
            StreamType::WEBRTC(video_stream_webrtc) => video_stream_webrtc,
            _any_other_stream_type => panic!("Failed to create WebRTC stream: {:?}.", result),
        };
        assert_eq!(
            result.pipeline(),
            concat!(
                "v4l2src device=/dev/video42",
                " ! video/x-h264,width=1080,height=720,framerate=30/1",
                " ! decodebin3",
                " ! videoconvert",
                " ! webrtcsink",
                " stun-server=stun://stun.l.google.com:19302",
                " turn-server=turn://test:1qaz2wsx@turn.homeneural.net:3478",
                " signaller::address=ws://192.168.3.4:44019/",
                " video-caps=video/x-h264,width=1080,height=720,framerate=30/1",
            )
        );
    }
}
