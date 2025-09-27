use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    thread,
};

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tracing::*;
use turn::{
    auth::*,
    relay::relay_static::*,
    server::{config::*, *},
    Error,
};
use webrtc_util::vnet::net::Net;

pub const DEFAULT_TURN_ENDPOINT: &str = "turn://user:pwd@0.0.0.0:3478";

#[derive(Debug)]
pub struct TurnServer {
    _handle: std::thread::JoinHandle<()>,
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

impl Default for TurnServer {
    #[instrument(level = "trace")]
    fn default() -> Self {
        Self {
            _handle: thread::Builder::new()
                .name("TurnServer".to_string())
                .spawn(TurnServer::run_main_loop)
                .expect("Failed spawning TurnServer thread"),
        }
    }
}

impl TurnServer {
    #[instrument(level = "debug")]
    fn run_main_loop() {
        let endpoint =
            match url::Url::parse(DEFAULT_TURN_ENDPOINT).context("Failed parsing endpoint") {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    error!("Failed parsing TurnServer url {DEFAULT_TURN_ENDPOINT:?}: {error:?}");
                    return;
                }
            };
        let realm = "some".into();

        debug!("Starting TURN server on {endpoint:?}...");

        tokio::runtime::Builder::new_multi_thread()
            .on_thread_start(|| debug!("Thread started"))
            .on_thread_stop(|| debug!("Thread stopped"))
            .thread_name_fn(|| {
                static ATOMIC_ID: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let id = ATOMIC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("TurnServer-{id}")
            })
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed building a new tokio runtime")
            .block_on(async move {
                match TurnServer::runner(endpoint.clone(), realm).await {
                    Ok(_) => debug!("TURN server successively Started!"),
                    Err(error) => error!("Error Starting TURN server on {endpoint:?}: {error:?}"),
                };
            });
    }

    #[instrument(level = "debug", fields(endpoint = %endpoint))]
    async fn runner(
        endpoint: url::Url,
        realm: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let realm = realm.clone();
        let public_ip = endpoint
            .host()
            .context(format!("Invalid host on {endpoint:?}."))?
            .to_string();
        let port = endpoint
            .port()
            .context(format!("Invalid port on {endpoint:?}."))?;
        let username = endpoint.username();
        let password = endpoint
            .password()
            .context(format!("Invalid password on {endpoint:?}."))?;

        // Cache -users flag for easy lookup later
        // If passwords are stored they should be saved to your DB hashed using turn.GenerateAuthKey
        let mut cred_map = HashMap::new();
        let key = generate_auth_key(username, &realm, password);
        cred_map.insert(username.to_owned(), key);

        // Create a UDP listener to pass into pion/turn
        // turn itself doesn't allocate any UDP sockets, but lets the user pass them in
        // this allows us to add logging, storage or modify inbound/outbound traffic
        let conn = Arc::new(UdpSocket::bind(format!("0.0.0.0:{port}")).await?);

        let _server = Server::new(ServerConfig {
            conn_configs: vec![ConnConfig {
                conn,
                relay_addr_generator: Box::new(RelayAddressGeneratorStatic {
                    relay_address: IpAddr::V4(public_ip.parse::<Ipv4Addr>()?),
                    address: public_ip,
                    net: Arc::new(Net::new(None)),
                }),
            }],
            realm: realm.to_owned(),
            auth_handler: Arc::new(MyAuthHandler::new(cred_map)),
            channel_bind_timeout: std::time::Duration::from_secs(0),
            alloc_close_notify: None,
        })
        .await
        .context("Error Creating the TURN server!")?;

        Ok(())
    }
}
