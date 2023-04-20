use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use gst_rtsp::RTSPLowerTrans;
use gst_rtsp_server::{prelude::*, RTSPTransportMode};
use tracing::*;

use crate::stream::rtsp::rtsp_media_factory;
use crate::video::types::VideoEncodeType;

#[allow(dead_code)]
pub struct RTSPServer {
    pub server: gst_rtsp_server::RTSPServer,
    host: String,
    port: u16,
    run: bool,
    pub path_to_factory: HashMap<String, rtsp_media_factory::Factory>,
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

    #[instrument(level = "debug")]
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

    #[instrument(level = "debug")]
    fn create_rtsp_bin(proxysink: &gst::Element, encode: &VideoEncodeType) -> Result<gst::Bin> {
        let description = match encode {
            VideoEncodeType::H264 => {
                concat!(
                    "proxysrc name=ProxySrc message-forward=true",
                    " ! queue leaky=downstream flush-on-eos=true max-size-buffers=0",
                    " ! rtph264depay",
                    " ! rtph264pay name=pay0 aggregate-mode=zero-latency config-interval=10 pt=96",
                )
            }
            VideoEncodeType::Yuyv => {
                concat!(
                    "proxysrc name=ProxySrc",
                    " ! queue leaky=downstream flush-on-eos=true max-size-buffers=0",
                    " ! rtpvrawdepay",
                    " ! rtpvrawpay name=pay0 pt=96",
                )
            }
            VideoEncodeType::Mjpg => {
                concat!(
                    "proxysrc name=ProxySrc",
                    " ! queue leaky=downstream flush-on-eos=true max-size-buffers=0",
                    " ! rtpjpegdepay",
                    " ! rtpjpegpay name=pay0 pt=96",
                )
            }
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for RTSP Sink"
                ))
            }
        };

        debug!("RTSP Server description: {description:#?}");

        let rtsp_bin = gst::parse_bin_from_description(&description, true)?;
        {
            let proxysrc = rtsp_bin
                .by_name("ProxySrc")
                .expect("Failed to find proxysrc by name: wrong name?");
            proxysrc.set_property("proxysink", proxysink);
            let _ = rtsp_bin.set_state(gst::State::Playing);
        }

        Ok(rtsp_bin)
    }

    #[instrument(level = "debug")]
    pub fn add_pipeline(
        path: &str,
        proxysink: &gst::Element,
        encoding: &VideoEncodeType,
    ) -> Result<()> {
        // Initialize the singleton before calling gst factory
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        let rtsp_bin = Self::create_rtsp_bin(proxysink, encoding)?;

        let factory = rtsp_media_factory::Factory::new(rtsp_bin);
        factory.set_shared(true);
        factory.set_buffer_size(0);
        factory.set_latency(0u32);
        factory.set_transport_mode(RTSPTransportMode::PLAY);
        factory.set_protocols(RTSPLowerTrans::UDP | RTSPLowerTrans::UDP_MCAST);

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

        Ok(())
    }
}
