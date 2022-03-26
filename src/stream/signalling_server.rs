use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Error;
use async_std::net::TcpListener;
use async_std::task;

use webrtcsink_signalling::handlers::Handler;
use webrtcsink_signalling::server::Server;

use log::*;

pub const DEFAULT_SIGNALLING_ENDPOINT: &str = "ws://0.0.0.0:6021";

#[allow(dead_code)]
pub struct SignallingServer {
    pub server: Server,
    endpoint: url::Url,
    run: bool,
    main_loop_thread: Option<std::thread::JoinHandle<()>>,
    main_loop_thread_rx_channel: std::sync::mpsc::Receiver<String>,
}

lazy_static! {
    pub static ref SIGNALLING_SERVER: Arc<Mutex<SignallingServer>> =
        Arc::new(Mutex::new(SignallingServer::default()));
}

impl SignallingServer {
    fn default() -> Self {
        let is_running = false;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();

        SignallingServer {
            server: Server::spawn(|stream| Handler::new(stream)),
            endpoint: url::Url::parse(DEFAULT_SIGNALLING_ENDPOINT).unwrap(),
            run: is_running,
            main_loop_thread: Some(thread::spawn(move || {
                SignallingServer::run_main_loop(sender)
            })),
            main_loop_thread_rx_channel: receiver,
        }
    }

    pub fn start() -> bool {
        SIGNALLING_SERVER.as_ref().lock().unwrap().run = true;
        true
    }

    pub fn is_running() -> bool {
        SIGNALLING_SERVER.as_ref().lock().unwrap().run
    }

    fn run_main_loop(_channel: std::sync::mpsc::Sender<String>) {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            debug!("Waiting for starting Signalling server...");
            if !SignallingServer::is_running() {
                continue;
            }

            debug!("Starting Signalling server...");
            match task::block_on(SignallingServer::signalling_server_tcp_runner()) {
                Ok(_) => (),
                Err(error) => error!("Error starting Signalling server! {error}"),
            }

            SIGNALLING_SERVER.as_ref().lock().unwrap().run = false;
            debug!("Signalling server stoppped!");
        }
    }

    async fn signalling_server_tcp_runner() -> Result<(), Error> {
        let signalling_server = SIGNALLING_SERVER.as_ref().lock().unwrap();
        let addr = format!(
            "{}:{}",
            signalling_server.endpoint.host().unwrap().to_string(),
            signalling_server.endpoint.port().unwrap()
        )
        .to_string()
        .parse::<SocketAddr>()?;
        drop(signalling_server);

        // Create the event loop and TCP listener we'll accept connections on.
        let listener = TcpListener::bind(&addr).await?;

        debug!("Signalling server: listening on: {}", addr);

        while let Ok((stream, _)) = listener.accept().await {
            let mut server_clone = SIGNALLING_SERVER.as_ref().lock().unwrap().server.clone();

            let address = match stream.peer_addr() {
                Ok(address) => address,
                Err(err) => {
                    warn!("Signalling server: connected peer with no address: {}", err);
                    continue;
                }
            };

            debug!("Signalling server: accepting connection from {}", address);

            task::spawn(async move { server_clone.accept_async(stream).await });
        }
        Ok(())
    }
}
