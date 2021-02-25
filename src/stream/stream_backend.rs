use super::types::*;
use super::video_stream_udp::VideoStreamUdp;
use crate::video::types::{VideoEncodeType, VideoSourceType};
use crate::video_stream::types::VideoAndStreamInformation;
use log::*;
use simple_error::SimpleError;
use url::Url;

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
    let encode = video_and_stream_information
        .stream_information
        .frame_size
        .encode
        .clone();
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
    let encode = video_and_stream_information
        .stream_information
        .frame_size
        .encode
        .clone();

    if let VideoEncodeType::UNKNOWN(name) = encode {
        return Err(SimpleError::new(format!(
            "Encode is not supported: {}",
            name
        )));
    }

    if VideoEncodeType::H264 != encode {
        return Err(SimpleError::new(format!(
            "Only H264 encode is supported now, used: {:?}",
            encode
        )));
    }

    return Ok(());
}

fn check_scheme(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<(), SimpleError> {
    let endpoints = &video_and_stream_information.stream_information.endpoints;
    let encode = video_and_stream_information
        .stream_information
        .frame_size
        .encode
        .clone();
    let scheme = endpoints.first().unwrap().scheme();

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

    return Ok(());
}

pub fn create_stream(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<StreamType, SimpleError> {
    let encode = video_and_stream_information
        .stream_information
        .frame_size
        .encode
        .clone();
    let endpoints = &video_and_stream_information.stream_information.endpoints;
    let frame_size = &video_and_stream_information.stream_information.frame_size;
    let video_source = &video_and_stream_information.video_source;

    // We only support local devices for now
    let VideoSourceType::Local(video_source) = video_source;
    let device = &video_source.device_path;

    if VideoEncodeType::H264 == encode {
        let video_format = format!(
            concat!(
                "v4l2src device={device}",
                " ! video/x-h264,width={width},height={height},framerate={framerate}/1",
            ),
            device = device,
            width = frame_size.width,
            height = frame_size.height,
            framerate = frame_size.frame_rate
        );

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
        println!("Created pipeline: {}", pipeline);
        let mut stream = VideoStreamUdp::default();
        stream.set_pipeline_description(&pipeline);
        return Ok(StreamType::UDP(stream));
    }

    return Err(SimpleError::new(format!(
        "Unsupported encode: {:?}",
        encode
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{
        types::FrameSize,
        video_source_local::{VideoSourceLocal, VideoSourceLocalType},
    };

    #[test]
    fn test_udp() {
        let result = create_stream(&VideoAndStreamInformation {
            name: "Test".into(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("udp://192.168.0.1:42").unwrap()],
                frame_size: FrameSize {
                    encode: VideoEncodeType::H264,
                    height: 720,
                    width: 1080,
                    frame_rate: 30,
                },
            },
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: "PotatoCam".into(),
                device_path: "/dev/video42".into(),
                typ: VideoSourceLocalType::Unknown("TestPotatoCam".into()),
            }),
        });

        assert!(result.is_ok());
        let result = result.unwrap();

        let StreamType::UDP(video_stream_udp) = result;
        assert_eq!(video_stream_udp.pipeline(), "v4l2src device=/dev/video42 ! video/x-h264,width=1080,height=720,framerate=30/1 ! h264parse ! queue ! rtph264pay config-interval=10 pt=96 ! multiudpsink clients=192.168.0.1:42");
    }
}
