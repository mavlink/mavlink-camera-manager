use regex::Regex;

mod helper;
mod mavlink_camera_information;

#[cfg(feature = "gst")]
use std::thread;
mod gst;

#[cfg(feature = "rtsp")]
pub fn start_rtsp_server(pipeline: &str, port: u16) {
    thread::spawn({
        let mut rtsp = gst::rtsp_server::RTSPServer::default();
        rtsp.set_pipeline(pipeline);
        rtsp.set_port(port);
        move || loop {
            rtsp.run_loop();
        }
    });
}

#[cfg(not(feature = "rtsp"))]
pub fn start_rtsp_server(_pipeline: &str, _port: u16) {}

#[cfg(feature = "gst")]
pub fn start_pipeline(pipeline: &str) {
    thread::spawn({
        let mut pipeline_runner = gst::pipeline_runner::PipelineRunner::default();
        pipeline_runner.set_pipeline(pipeline);
        move || loop {
            pipeline_runner.run_loop();
        }
    });
}

#[cfg(not(feature = "gst"))]
pub fn start_pipeline(_pipeline: &str) {}

fn main() {
    let matches = helper::get_clap_matches();
    let verbose = matches.is_present("verbose");
    let connection_string = matches.value_of("connect").unwrap();
    println!("MAVLink connection string: {}", connection_string);

    // Create mavlink camera manager
    let mut mavlink_camera = mavlink_camera_information::MavlinkCameraInformation::default();
    mavlink_camera.connect(connection_string);
    mavlink_camera.set_verbosity(verbose);

    // Set a default value
    let mut video_stream_uri = format!("udp://0.0.0.0:5600");

    if matches.is_present("endpoint") {
        video_stream_uri = matches.value_of("endpoint").unwrap().to_string();
    }

    if cfg!(feature = "rtsp") {
        let pipeline_string = matches.value_of("pipeline-rtsp").unwrap();
        let rtsp_port = matches
            .value_of("port")
            .unwrap()
            .parse::<u16>()
            .unwrap_or_else(|error| panic!("Invalid RTSP port: {}", error));

        start_rtsp_server(pipeline_string, rtsp_port);

        // Look for valid ips with our use (192.168.(2).1)
        // If no valid ip address is found, the first one that matches the regex is used
        let regex = Regex::new(r"192.168.(\d{1})\..+$").unwrap();
        let mut video_stream_ip = String::new();
        let ips = helper::get_valid_ip_address();

        if ips.is_empty() {
            video_stream_uri = format!("rtsp://0.0.0.0:{}/video1", rtsp_port);
        } else {
            for ip in ips {
                let ip = ip.to_string();

                if !regex.is_match(&ip) {
                    continue;
                }

                // Check if we have a valid ip address
                // And force update if we are inside companion ip address range
                let capture = regex.captures(&ip).unwrap();
                if video_stream_ip.is_empty() {
                    video_stream_ip = String::from(&ip);
                }
                if &capture[1] == "2" {
                    video_stream_ip = String::from(&ip);
                }
                video_stream_uri = format!("rtsp://{}:{}/video1", video_stream_ip, rtsp_port);
            }
        }
    } else if cfg!(feature = "gst") {
        let pipeline_string = matches.value_of("pipeline").unwrap();
        start_pipeline(pipeline_string);
    }

    println!("Stream will be available in: {}", &video_stream_uri);

    mavlink_camera.set_video_stream_uri(video_stream_uri);

    loop {
        mavlink_camera.run_loop();
    }
}
