use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use gst_rtsp::RTSPLowerTrans;
use gst_rtsp_server::{prelude::*, RTSPTransportMode};
use tracing::*;

use super::rtsp_scheme::RTSPScheme;

#[allow(dead_code)]
pub struct RTSPServer {
    pub server: gst_rtsp_server::RTSPServer,
    host: String,
    port: u16,
    run: bool,
    pub path_to_factory: HashMap<String, gst_rtsp_server::RTSPMediaFactory>,
    main_loop_thread: Option<std::thread::JoinHandle<()>>,
    main_loop_thread_rx_channel: std::sync::mpsc::Receiver<String>,
}

lazy_static! {
    pub static ref RTSP_SERVER: Arc<Mutex<RTSPServer>> =
        Arc::new(Mutex::new(RTSPServer::default()));
}

pub const RTSP_SERVER_PORT: u16 = 8554;

impl RTSPServer {
    #[instrument(level = "debug")]
    fn default() -> Self {
        if let Err(error) = gst::init() {
            error!("Failed to init GStreamer: {error:?}");
        }

        let is_running = false;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();

        let host = "0.0.0.0".to_string();
        let port = RTSP_SERVER_PORT;
        let server = gst_rtsp_server::RTSPServer::new();
        server.set_address(&host);
        server.set_service(&port.to_string());

        RTSPServer {
            server,
            host,
            port,
            run: is_running,
            path_to_factory: HashMap::new(),
            main_loop_thread: Some(
                thread::Builder::new()
                    .name("RTSPServer".to_string())
                    .spawn(move || RTSPServer::run_main_loop(sender))
                    .expect("Failed when spawing RTSPServer thread"),
            ),
            main_loop_thread_rx_channel: receiver,
        }
    }

    #[instrument(level = "debug")]
    pub fn is_running() -> bool {
        RTSP_SERVER.as_ref().lock().unwrap().run
    }

    #[instrument(level = "debug", skip(channel))]
    fn run_main_loop(channel: std::sync::mpsc::Sender<String>) {
        if let Err(error) = gst::init() {
            let _ = channel.send(format!("Failed to init GStreamer: {error:?}"));
            return;
        }

        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if !RTSPServer::is_running() {
                continue;
            }

            let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

            // Attach the server to our main context.
            // A main context is the thing where other stuff is registering itself for its
            // events (e.g. sockets, GStreamer bus, ...) and the main loop is something that
            // polls the main context for its events and dispatches them to whoever is
            // interested in them. In this example, we only do have one, so we can
            // leave the context parameter empty, it will automatically select
            // the default one.
            let id = rtsp_server
                .server
                .attach(None)
                .expect("Failed attaching to default context");

            // Start the mainloop. From this point on, the server will start to serve
            // our quality content to connecting clients.
            let main_loop = gst::glib::MainLoop::new(None, false);
            rtsp_server.run = true;
            drop(rtsp_server);

            main_loop.run();

            let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();
            rtsp_server.run = false;

            id.remove();
        }
    }

    #[instrument(level = "debug", skip(rtsp_appsrc, pts_offset))]
    pub fn add_pipeline(
        scheme: &RTSPScheme,
        path: &str,
        rtsp_appsrc: std::sync::Arc<std::sync::Mutex<Option<gst_app::AppSrc>>>,
        pts_offset: std::sync::Arc<std::sync::Mutex<Option<gst::ClockTime>>>,
        video_caps: &gst::Caps,
        rtp_queue_time_ns: u64,
    ) -> Result<()> {
        // Initialize the singleton before calling gst factory
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        let protocols = match scheme {
            RTSPScheme::Rtsp => {
                RTSPLowerTrans::UDP | RTSPLowerTrans::UDP_MCAST | RTSPLowerTrans::TCP
            }
            RTSPScheme::Rtspu => RTSPLowerTrans::UDP | RTSPLowerTrans::UDP_MCAST,
            RTSPScheme::Rtspt => RTSPLowerTrans::TCP,
            RTSPScheme::Rtsph => RTSPLowerTrans::HTTP | RTSPLowerTrans::TCP,
            RTSPScheme::Rtsps => {
                RTSPLowerTrans::TCP
                    | RTSPLowerTrans::UDP
                    | RTSPLowerTrans::UDP_MCAST
                    | RTSPLowerTrans::TLS
            }
            RTSPScheme::Rtspsu => {
                RTSPLowerTrans::UDP | RTSPLowerTrans::UDP_MCAST | RTSPLowerTrans::TLS
            }
            RTSPScheme::Rtspst => RTSPLowerTrans::TCP | RTSPLowerTrans::TLS,
            RTSPScheme::Rtspsh => RTSPLowerTrans::HTTP | RTSPLowerTrans::TCP | RTSPLowerTrans::TLS,
        };

        let factory = gst_rtsp_server::RTSPMediaFactory::new();
        factory.set_shared(true);
        factory.set_buffer_size(0);
        factory.set_latency(0u32);
        factory.set_transport_mode(RTSPTransportMode::PLAY);
        factory.set_protocols(protocols);

        let caps_name = video_caps
            .structure(0)
            .map(|s| s.name().to_string())
            .unwrap_or_default();

        let description = match caps_name.as_str() {
            "video/x-h264" => {
                format!(
                    concat!(
                        "appsrc name=source is-live=true format=time do-timestamp=false",
                        " ! queue leaky=downstream flush-on-eos=true silent=true max-size-buffers=0 max-size-bytes=0 max-size-time={queue_time}",
                        " ! rtph264pay name=pay0 aggregate-mode=zero-latency config-interval=-1 pt=96",
                    ),
                    queue_time = rtp_queue_time_ns,
                )
            }
            "video/x-h265" => {
                format!(
                    concat!(
                        "appsrc name=source is-live=true format=time do-timestamp=false",
                        " ! queue leaky=downstream flush-on-eos=true silent=true max-size-buffers=0 max-size-bytes=0 max-size-time={queue_time}",
                        " ! rtph265pay name=pay0 aggregate-mode=zero-latency config-interval=-1 pt=96",
                    ),
                    queue_time = rtp_queue_time_ns,
                )
            }
            "video/x-raw" => {
                format!(
                    concat!(
                        "appsrc name=source is-live=true format=time do-timestamp=false",
                        " ! queue leaky=downstream flush-on-eos=true silent=true max-size-buffers=0 max-size-bytes=0 max-size-time={queue_time}",
                        " ! rtpvrawpay name=pay0 pt=96",
                    ),
                    queue_time = rtp_queue_time_ns,
                )
            }
            "image/jpeg" => {
                format!(
                    concat!(
                        "appsrc name=source is-live=true format=time do-timestamp=false",
                        " ! queue leaky=downstream flush-on-eos=true silent=true max-size-buffers=0 max-size-bytes=0 max-size-time={queue_time}",
                        " ! rtpjpegpay name=pay0 pt=96",
                    ),
                    queue_time = rtp_queue_time_ns,
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Video caps {unsupported:?} not supported for RTSP Sink"
                ))
            }
        };

        debug!("RTSP Server description: {description:#?}");

        factory.set_launch(&description);

        let video_caps_clone = video_caps.clone();
        factory.connect_media_configure(move |_factory, media| {
            let element = media.element();
            if let Some(bin) = element.downcast_ref::<gst::Bin>() {
                if let Some(source) = bin.by_name("source") {
                    if let Ok(appsrc) = source.downcast::<gst_app::AppSrc>() {
                        appsrc.set_caps(Some(&video_caps_clone));
                        appsrc.set_max_bytes(0);
                        appsrc.set_property("block", false);
                        // Reset PTS offset so timestamps rebase from 0 for the new media
                        *pts_offset.lock().unwrap() = None;
                        *rtsp_appsrc.lock().unwrap() = Some(appsrc);
                        debug!("Connected appsrc for RTSP media");
                    }
                } else {
                    error!("Failed to find 'source' appsrc in RTSP media pipeline");
                }
            } else {
                error!("Failed to downcast RTSP media element to Bin");
            }
        });

        if let Some(server) = rtsp_server
            .path_to_factory
            .insert(path.to_string(), factory)
        {
            return Err(anyhow!("Error: required path already exists! The older was updated with the new configurations: {server:#?}"));
        }

        Ok(())
    }

    #[instrument(level = "debug")]
    pub fn start_pipeline(path: &str) -> Result<()> {
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        // Much like HTTP servers, RTSP servers have multiple endpoints that
        // provide different streams. Here, we ask our server to give
        // us a reference to his list of endpoints, so we can add our
        // test endpoint, providing the pipeline from the cli.
        let mounts = rtsp_server
            .server
            .mount_points()
            .context("Could not get mount points")?;

        let factory = rtsp_server
            .path_to_factory
            .get(path)
            .context(format!(
                "Factory for path {path:?} not found in RTSP factories"
            ))?
            .to_owned();

        // Now we add a new mount-point and tell the RTSP server to serve the content
        // provided by the factory we configured above, when a client connects to
        // this specific path.
        mounts.add_factory(path, factory);

        rtsp_server.run = true; // start the main loop thread

        Ok(())
    }

    #[instrument(level = "debug")]
    pub fn stop_pipeline(path: &str) -> Result<()> {
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        if !rtsp_server.path_to_factory.contains_key(path) {
            return Err(anyhow!("Path {path:?} not known."));
        }

        // Much like HTTP servers, RTSP servers have multiple endpoints that
        // provide different streams. Here, we ask our server to give
        // us a reference to his list of endpoints, so we can add our
        // test endpoint, providing the pipeline from the cli.
        let mounts = rtsp_server
            .server
            .mount_points()
            .context("Could not get mount points")?;

        mounts.remove_factory(path);

        rtsp_server.path_to_factory.remove(path);

        debug!("RTSP {path:?} removed.");

        Ok(())
    }
}
