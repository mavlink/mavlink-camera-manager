use clap;

pub fn get_valid_ip_address() -> Vec<std::net::IpAddr> {
    let mut ips = Vec::new();
    for interface in pnet::datalink::interfaces() {
        for interface_ip in interface.ips {
            if !interface_ip.is_ipv4() {
                continue;
            }
            ips.push(interface_ip.ip());
        }
    }
    return ips;
}

pub fn get_clap_matches<'a>() -> clap::ArgMatches<'a> {
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
                .conflicts_with_all(&["pipeline", "pipeline-rtsp", "port"]),
        )
        .arg(
            clap::Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Be verbose")
                .takes_value(false),
        );

    if cfg!(feature = "gst") {
        matches = matches.arg(
            clap::Arg::with_name("pipeline")
                .long("pipeline")
                .value_name("GSTREAMER_PIPELINE")
                .help("Gstreamer pipeline that ends with a sink type.")
                .takes_value(true)
                .conflicts_with_all(&["pipeline-rtsp", "port"])
                .default_value("videotestsrc ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc ! rtph264pay ! udpsink host=0.0.0.0 port=5600"),
        )
    }

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

    return matches.get_matches();
}
