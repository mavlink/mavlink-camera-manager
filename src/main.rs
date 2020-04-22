use clap;
use pnet;
use regex::Regex;
use std::thread;

mod mavlink_camera_information;

#[cfg(feature = "rtsp")]
mod rtsp;

fn main() {
    let matches = clap::App::new(env!("CARGO_PKG_NAME"))
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
            clap::Arg::with_name("pipeline")
                .long("pipeline")
                .value_name("GSTREAMER_PIPELINE")
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
        )
        .arg(
            clap::Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Be verbose")
                .takes_value(false),
        )
        .get_matches();

    let verbose = matches.is_present("verbose");
    let connection_string = matches.value_of("connect").unwrap();
    let pipeline_string = matches.value_of("pipeline").unwrap();
    let rtsp_port = matches
        .value_of("port")
        .unwrap()
        .parse::<u16>()
        .unwrap_or_else(|error| panic!("Invalid RTSP port: {}", error));

    println!("MAVLink connection string: {}", connection_string);

    // Create mavlink camera manager
    let mut mavlink_camera = mavlink_camera_information::MavlinkCameraInformation::default();
    mavlink_camera.connect(connection_string);
    mavlink_camera.set_verbosity(verbose);

    let mut rtsp = rtsp::rtsp_server::RTSPServer::default();
    rtsp.set_pipeline(pipeline_string);
    rtsp.set_port(rtsp_port);

    println!("Stream will be available in:");

    // Look for valid ips with our use (192.168.(2).1)
    // If no valid ip address is found, the first one that matches the regex is used
    let regex = Regex::new(r"192.168.(\d{1})\..+$").unwrap();
    let mut video_stream_ip = String::new();

    for interface in pnet::datalink::interfaces() {
        for interface_ip in interface.ips {
            if !interface_ip.is_ipv4() {
                continue;
            }
            let ip = interface_ip.ip().to_string();

            if !regex.is_match(&ip) {
                continue;
            }

            println!("\trtsp://{}:{}/video1", &ip, &rtsp_port);

            // Check if we have a valid ip address
            // And force update if we are inside companion ip address range
            let capture = regex.captures(&ip).unwrap();
            if video_stream_ip.is_empty() {
                video_stream_ip = String::from(&ip);
            }
            if &capture[1] == "2" {
                video_stream_ip = String::from(&ip);
            }
        }
    }

    mavlink_camera.set_video_stream_uri(format!("rtsp://{}:{}/video1", video_stream_ip, rtsp_port));

    thread::spawn({
        move || loop {
            mavlink_camera.run_loop();
        }
    });

    loop {
        rtsp.run_loop();
    }
}
