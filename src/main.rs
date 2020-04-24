use clap;
use regex::Regex;

mod helper;
mod mavlink_camera_information;

#[cfg(feature = "rtsp")]
mod rtsp;

#[cfg(feature = "rtsp")]
use std::thread;

#[cfg(feature = "rtsp")]
pub fn start_rtsp_server(pipeline: &str, port: u16) {
    thread::spawn({
        let mut rtsp = rtsp::rtsp_server::RTSPServer::default();
        rtsp.set_pipeline(pipeline);
        rtsp.set_port(port);
        move || loop {
            rtsp.run_loop();
        }
    });
}

#[cfg(not(feature = "rtsp"))]
pub fn start_rtsp_server(_pipeline: &str, _port: u16) {}

fn main() {
    let mut matches = clap::App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .arg(
            clap::Arg::with_name("connect")
                .short("c")
                .long("connect")
                .value_name("TYPE:<IP/SERIAL>:<PORT/BAUDRATE>")
                .help("Sets the mavlink connection string")
                .takes_value(true)
                .default_value("udpout:0.0.0.0:14550"),
        )
        .arg(
            clap::Arg::with_name("endpoint")
                .long("endpoint")
                .value_name("TYPE://IP:PORT[PATH]")
                .long_help(
                    "Video endpoint provided by someone.
TYPE should be:
\t- rtsp (Can have PATH)
\t- udp (x264)
\t- udp265 (x265)
\t- mpegts (UDP MPEG)
\t- tcp (MPEG)
\t- tsusb (taisync)
Example of valid arguments:
\t- udp:://192.168.2.2:5600
\t- rtsp://0.0.0.0:8554/video1",
                )
                .takes_value(true)
                .conflicts_with_all(&["pipeline-rtsp", "port"]),
        )
        .arg(
            clap::Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Be verbose")
                .takes_value(false),
        );

    if cfg!(feature = "rtsp") {
        matches = matches.arg(
            clap::Arg::with_name("pipeline-rtsp")
                .long("pipeline-rtsp")
                .value_name("RTSP_GSTREAMER_PIPELINE")
                .help("Gstreamer pipeline that ends with 'rtph264pay name=pay0'")
                .takes_value(true)
                .default_value("videotestsrc ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc ! rtph264pay name=pay0"),
        )
        .arg(
            clap::Arg::with_name("port")
                .short("p")
                .long("port")
                .value_name("RTSP_PORT_NUMBER")
                .help("RTSP server port")
                .takes_value(true)
                .default_value("8554"),
        );
    }

    let matches = matches.get_matches();
    let verbose = matches.is_present("verbose");
    let connection_string = matches.value_of("connect").unwrap();
    println!("MAVLink connection string: {}", connection_string);

    // Create mavlink camera manager
    let mut mavlink_camera = mavlink_camera_information::MavlinkCameraInformation::default();
    mavlink_camera.connect(connection_string);
    mavlink_camera.set_verbosity(verbose);

    // Set a default value
    let mut video_stream_uri = format!("udp://0.0.0.0:5600");

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
            video_stream_uri = matches.value_of("endpoint").unwrap();
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
    }

    println!("Stream will be available in: {}", &video_stream_uri);

    mavlink_camera.set_video_stream_uri(video_stream_uri);

    loop {
        mavlink_camera.run_loop();
    }
}
