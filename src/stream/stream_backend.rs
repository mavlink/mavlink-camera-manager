use super::types::*;
use super::video_stream_redirect::VideoStreamRedirect;
use super::video_stream_rtsp::VideoStreamRtsp;
use super::video_stream_udp::VideoStreamUdp;
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

    let endpoints_have_same_scheme = endpoints
        .windows(2)
        .all(|win| win[0].scheme() == win[1].scheme());
    if !endpoints_have_same_scheme {
        return Err(SimpleError::new(format!(
            "Endpoints scheme are not the same: {:#?}",
            endpoints
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
            something => Err(SimpleError::new(format!(
                "Unsupported scheme: {}",
                something
            ))),
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
}
