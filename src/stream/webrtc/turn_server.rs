use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;

use tokio::net::UdpSocket;
use tokio::runtime::Runtime;

use util::vnet::net::Net;

use turn::auth::*;
use turn::relay::relay_static::*;
use turn::server::{config::*, *};
use turn::Error;

use log::*;

pub const DEFAULT_TURN_ENDPOINT: &str = "turn://user:pwd@0.0.0.0:3478";
pub const DEFAULT_STUN_ENDPOINT: &str = "stun://0.0.0.0:3478";

#[allow(dead_code)]
pub struct TurnServer {
    server: Option<turn::server::Server>,
    endpoint: url::Url,
    realm: String,
    run: bool,
    running_peers: u32,
    main_loop_thread: Option<std::thread::JoinHandle<()>>,
    main_loop_thread_rx_channel: std::sync::mpsc::Receiver<String>,
}

struct MyAuthHandler {
    cred_map: HashMap<String, Vec<u8>>,
}

impl MyAuthHandler {
    fn new(cred_map: HashMap<String, Vec<u8>>) -> Self {
        MyAuthHandler { cred_map }
    }
}

impl AuthHandler for MyAuthHandler {
    fn auth_handle(
        &self,
        username: &str,
        _realm: &str,
        _src_addr: SocketAddr,
    ) -> Result<Vec<u8>, Error> {
        if let Some(pw) = self.cred_map.get(username) {
            debug!("username={}, password={:?}", username, pw);
            Ok(pw.to_vec())
        } else {
            Err(Error::ErrFakeErr)
        }
    }
}

lazy_static! {
    pub static ref TURN_SERVER: Arc<Mutex<TurnServer>> =
        Arc::new(Mutex::new(TurnServer::default()));
}

impl TurnServer {
    fn default() -> Self {
        match gstreamer::init() {
            Ok(_) => {}
            Err(error) => error!("Error! {error}"),
        }

        let is_running = false;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();

        TurnServer {
            server: None,
            endpoint: url::Url::parse(DEFAULT_TURN_ENDPOINT).unwrap(),
            realm: "some".into(),
            run: is_running,
            running_peers: 0,
            main_loop_thread: Some(thread::spawn(move || TurnServer::run_main_loop(sender))),
            main_loop_thread_rx_channel: receiver,
        }
    }

    pub fn notify_start() {
        debug!("TURN server was notified with a Start!");
        let mut turn_server = TURN_SERVER.as_ref().lock().unwrap();
        turn_server.run = true;
        turn_server.running_peers += 1;
    }

    pub fn notify_stop() {
        debug!("TURN server was notified with a Stop!");
        let mut turn_server = TURN_SERVER.as_ref().lock().unwrap();

        if turn_server.running_peers != 0 {
            turn_server.running_peers -= 1;
        }

        if turn_server.running_peers == 0 {
            turn_server.run = false;
        }
    }

    pub fn is_running() -> bool {
        TURN_SERVER.as_ref().lock().unwrap().run
    }

    fn run_main_loop(channel: std::sync::mpsc::Sender<String>) {
        if let Err(error) = gstreamer::init() {
            let _ = channel.send(format!("Failed to init GStreamer: {error}"));
            return;
        }

        loop {
            debug!("Waiting for starting TURN server...");
            while !TurnServer::is_running() {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }

            debug!("Starting TURN server...");
            let runtime = Runtime::new().unwrap();
            match runtime.block_on(TurnServer::turn_server_udp_runner()) {
                Ok(_) => debug!("TURN server successively Started!"),
                Err(error) => error!("Error Starting TURN server! {error}"),
            };

            while TurnServer::is_running() {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }

            debug!("Stopping TURN server...");
            let turn_server = TURN_SERVER.as_ref().lock().unwrap();
            match runtime.block_on(turn_server.server.as_ref().unwrap().close()) {
                Ok(_) => debug!("TURN server successively Stoppped!"),
                Err(error) => error!("Error Stopping TURN server! {error}"),
            };
        }
    }

    async fn turn_server_udp_runner(
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut turn_server = TURN_SERVER.as_ref().lock().unwrap();

        let realm = turn_server.realm.clone();
        let public_ip = turn_server.endpoint.host().unwrap().to_string();
        let port = turn_server.endpoint.port().unwrap();
        let username = turn_server.endpoint.username();
        let password = turn_server.endpoint.password().unwrap();

        // Cache -users flag for easy lookup later
        // If passwords are stored they should be saved to your DB hashed using turn.GenerateAuthKey
        let mut cred_map = HashMap::new();
        let key = generate_auth_key(&username, &realm, &password);
        cred_map.insert(username.to_owned(), key);

        // Create a UDP listener to pass into pion/turn
        // turn itself doesn't allocate any UDP sockets, but lets the user pass them in
        // this allows us to add logging, storage or modify inbound/outbound traffic
        let conn = Arc::new(UdpSocket::bind(format!("0.0.0.0:{}", &port)).await?);

        let server = Server::new(ServerConfig {
            conn_configs: vec![ConnConfig {
                conn,
                relay_addr_generator: Box::new(RelayAddressGeneratorStatic {
                    relay_address: IpAddr::V4(public_ip.parse::<Ipv4Addr>().unwrap()),
                    address: public_ip,
                    net: Arc::new(Net::new(None)),
                }),
            }],
            realm: realm.to_owned(),
            auth_handler: Arc::new(MyAuthHandler::new(cred_map)),
            channel_bind_timeout: std::time::Duration::from_secs(0),
        })
        .await
        .expect("Error Creating the TURN server!");

        turn_server.server = Some(server);
        drop(turn_server);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TurnServer;

    #[test]
    // Note: sometimes this test fails.
    fn test_notify_start_stop_logic() {
        assert!(!TurnServer::is_running());

        // The TURN server should be running if one or more pipelines added
        TurnServer::notify_start();
        assert!(TurnServer::is_running());
        TurnServer::notify_start();
        assert!(TurnServer::is_running());

        // Remove one pipeline, it should keep running
        TurnServer::notify_stop();
        assert!(TurnServer::is_running());

        // Remove the last pipeline, it should stop running
        TurnServer::notify_stop();
        assert!(!TurnServer::is_running());

        // After stopping, it should start again if a pipeline added
        TurnServer::notify_start();
        assert!(TurnServer::is_running());
    }
}
