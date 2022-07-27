use std::sync::{Arc, Mutex};
use std::thread;

use std::collections::HashMap;

use glib;
use gstreamer_rtsp_server;
use gstreamer_rtsp_server::prelude::{
    RTSPMediaFactoryExt, RTSPMountPointsExt, RTSPServerExt, RTSPServerExtManual,
};
use simple_error::{simple_error, SimpleResult};

#[allow(dead_code)]
pub struct RTSPServer {
    pub server: gstreamer_rtsp_server::RTSPServer,
    host: String,
    port: u16,
    run: bool,
    pub path_to_factory: HashMap<String, gstreamer_rtsp_server::RTSPMediaFactory>,
    main_loop_thread: Option<std::thread::JoinHandle<()>>,
    main_loop_thread_rx_channel: std::sync::mpsc::Receiver<String>,
}

lazy_static! {
    pub static ref RTSP_SERVER: Arc<Mutex<RTSPServer>> =
        Arc::new(Mutex::new(RTSPServer::default()));
}

impl RTSPServer {
    fn default() -> Self {
        match gstreamer::init() {
            Ok(_) => {}
            Err(error) => println!("Error! {}", error),
        }

        let is_running = false;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();

        RTSPServer {
            server: gstreamer_rtsp_server::RTSPServer::new(),
            host: "0.0.0.0".into(),
            port: 8554,
            run: is_running,
            path_to_factory: HashMap::new(),
            main_loop_thread: Some(thread::spawn(move || RTSPServer::run_main_loop(sender))),
            main_loop_thread_rx_channel: receiver,
        }
    }

    pub fn is_running() -> bool {
        RTSP_SERVER.as_ref().lock().unwrap().run
    }

    fn run_main_loop(channel: std::sync::mpsc::Sender<String>) {
        if let Err(error) = gstreamer::init() {
            let _ = channel.send(format!("Failed to init GStreamer: {}", error));
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
            let id = rtsp_server.server.attach(None).unwrap();

            // Start the mainloop. From this point on, the server will start to serve
            // our quality content to connecting clients.
            let main_loop = glib::MainLoop::new(None, false);
            rtsp_server.run = true;
            drop(rtsp_server);

            main_loop.run();

            let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();
            rtsp_server.run = false;

            id.remove();
        }
    }

    pub fn add_pipeline(pipeline_description: &str, path: &str) -> SimpleResult<()> {
        // Initialize the singleton before calling gstreamer factory
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        let factory = gstreamer_rtsp_server::RTSPMediaFactory::new();
        factory.set_launch(&pipeline_description);
        factory.set_shared(true);

        match rtsp_server
            .path_to_factory
            .insert(path.to_string(), factory)
        {
            Some(server) => Err(simple_error!(format!("Error: required path already exists! The older was updated with the new configurations: {server:#?}"))),
            None => Ok(())
        }
    }

    fn configure(host: &str, port: u16) {
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();
        if rtsp_server.run {
            return;
        }
        rtsp_server.host = host.to_string();
        rtsp_server.port = port;

        rtsp_server.server.set_address(&rtsp_server.host);

        rtsp_server
            .server
            .set_service(&rtsp_server.port.to_string());
    }

    pub fn start_pipeline(path: &str) {
        RTSPServer::configure("0.0.0.0".into(), 8554);

        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        // Much like HTTP servers, RTSP servers have multiple endpoints that
        // provide different streams. Here, we ask our server to give
        // us a reference to his list of endpoints, so we can add our
        // test endpoint, providing the pipeline from the cli.
        let mounts = rtsp_server
            .server
            .mount_points()
            .ok_or("Could not get mount points")
            .unwrap();

        let factory = rtsp_server.path_to_factory.get(path).unwrap();

        // Now we add a new mount-point and tell the RTSP server to serve the content
        // provided by the factory we configured above, when a client connects to
        // this specific path.
        mounts.add_factory(path, factory);

        rtsp_server.run = true; // start the main loop thread
    }

    pub fn stop_pipeline(path: &str) {
        let mut rtsp_server = RTSP_SERVER.as_ref().lock().unwrap();

        // Much like HTTP servers, RTSP servers have multiple endpoints that
        // provide different streams. Here, we ask our server to give
        // us a reference to his list of endpoints, so we can add our
        // test endpoint, providing the pipeline from the cli.
        let mounts = rtsp_server
            .server
            .mount_points()
            .ok_or("Could not get mount points")
            .unwrap();

        mounts.remove_factory(path);

        rtsp_server.path_to_factory.remove(path);
        // TODO: call mainloop.quit() to stop the server if there is no endpoints
        // if rtsp_server.path_to_factory.is_empty() {...}
    }
}
