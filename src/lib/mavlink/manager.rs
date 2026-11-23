use std::{
    marker::Send,
    sync::{Arc, Mutex, RwLock},
};

use mavlink::{MavConnection, MavHeader, common::MavMessage};

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tracing::*;

use crate::settings;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Default::default();
}

pub struct Manager {
    connection: Arc<RwLock<Connection>>,
    ids: Arc<RwLock<Vec<u8>>>,
}

struct Connection {
    address: String,
    state: ConnectionState,
    generation: u64,
    sender: broadcast::Sender<Message>,
}

enum ConnectionState {
    Disconnected,
    Connecting,
    Connected(Box<dyn MavConnection<MavMessage> + Sync + Send>),
}

#[derive(Debug, Clone)]
pub enum Message {
    Received((MavHeader, MavMessage)),
    ToBeSent((MavHeader, MavMessage)),
}

impl Default for Manager {
    #[instrument(level = "debug")]
    fn default() -> Self {
        let address = settings::manager::mavlink_endpoint();

        let (sender, _receiver) = broadcast::channel(100);

        let this = Self {
            connection: Arc::new(RwLock::new(Connection {
                address,
                state: ConnectionState::Disconnected,
                generation: 0,
                sender,
            })),
            ids: Arc::new(RwLock::new(vec![])),
        };

        let connection = this.connection.clone();
        std::thread::Builder::new()
            .name("MavSender".into())
            .spawn(move || Manager::sender_loop(connection))
            .expect("Failed to spawn MavSender thread");

        let connection = this.connection.clone();
        std::thread::Builder::new()
            .name("MavReceiver".into())
            .spawn(move || Manager::receiver_loop(connection))
            .expect("Failed to spawn MavReceiver thread");

        this
    }
}

impl Manager {
    // Construct our manager, should be done inside main
    #[instrument(level = "debug")]
    pub fn init() {
        MANAGER.as_ref();
    }

    #[instrument(level = "debug", skip(inner))]
    fn receiver_loop(inner: Arc<RwLock<Connection>>) {
        let mut generation = 0;

        loop {
            loop {
                let Ok(inner_guard) = inner.read() else {
                    break; // Break to trigger reconnection
                };
                generation = inner_guard.generation;
                let ConnectionState::Connected(mavlink) = &inner_guard.state else {
                    break; // Break to trigger reconnection
                };

                // Receive from the Mavlink network
                let (header, message) = match mavlink.recv() {
                    Ok(message) => message,
                    Err(error) => {
                        trace!("Failed receiving from mavlink: {error:?}");

                        match &error {
                            mavlink::error::MessageReadError::Parse(_) => continue,
                            mavlink::error::MessageReadError::Io(io_error) => {
                                // The mavlink connection is handled by the sender_loop, so we can just silently skip the WouldBlocks
                                if io_error.kind() == std::io::ErrorKind::WouldBlock {
                                    continue;
                                }
                            }
                        }

                        error!("Failed receiving message from Mavlink Connection: {error:?}");
                        break; // Break to trigger reconnection
                    }
                };

                trace!("Message received: {header:?}, {message:?}");

                // Early filter non-GCS messages to avoid passing unwanted ones to the camera componenets.
                let allowed_component_ids = [
                    mavlink::common::MavComponent::MAV_COMP_ID_ALL as u8,
                    mavlink::common::MavComponent::MAV_COMP_ID_SYSTEM_CONTROL as u8,
                    mavlink::common::MavComponent::MAV_COMP_ID_MISSIONPLANNER as u8,
                ];
                if !allowed_component_ids.contains(&header.component_id) {
                    trace!("Message dropped: {header:?}, {message:?}");
                    continue;
                }

                trace!("Message accepted: {header:?}, {message:?}");

                // Send the received message to the cameras
                if let Err(error) = inner_guard
                    .sender
                    .send(Message::Received((header, message)))
                {
                    error!("Failed handling message: {error:?}");
                    continue;
                }
            }

            Self::reconnect(&inner, generation);

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    #[instrument(level = "debug", skip(inner))]
    fn sender_loop(inner: Arc<RwLock<Connection>>) {
        let mut receiver = { inner.read().unwrap().sender.subscribe() };
        let mut generation = 0;
        let mut pending = None;

        loop {
            loop {
                // Receive answer from the cameras
                let message = match pending.take() {
                    Some(message) => message,
                    None => match receiver.blocking_recv() {
                        Ok(Message::ToBeSent(message)) => message,
                        Ok(Message::Received(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => {
                            unreachable!(
                                "Closed channel: This should never happen, this channel is static!"
                            );
                        }
                        Err(broadcast::error::RecvError::Lagged(samples)) => {
                            warn!(
                                "Channel is lagged behind by {samples} messages. Expect degraded performance on the mavlink responsiviness."
                            );
                            continue;
                        }
                    },
                };
                let (header, message) = pending.insert(message);

                let Ok(inner_guard) = inner.read() else {
                    break; // Break to trigger reconnection
                };
                generation = inner_guard.generation;
                let ConnectionState::Connected(mavlink) = &inner_guard.state else {
                    break; // Break to trigger reconnection
                };

                // Send the response from the cameras to the Mavlink network
                if let Err(error) = mavlink.send(header, message) {
                    error!("Failed sending message to Mavlink Connection: {error:?}");

                    pending = None;
                    break; // Break to trigger reconnection
                }

                debug!("Message sent: {header:?}, {message:?}");
                pending = None;
            }

            Self::reconnect(&inner, generation);

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    /// Rebinds the shared connection, unless the other loop is already doing it or has already
    /// replaced the `failed_generation` connection.
    ///
    /// The old connection is dropped before connecting because it still owns the socket: binding
    /// the same `udpin` address twice fails with EADDRINUSE, retrying forever. The lock is
    /// released while connecting so that API callers reading the sender don't block on it.
    #[instrument(level = "debug", skip(inner))]
    fn reconnect(inner: &Arc<RwLock<Connection>>, failed_generation: u64) {
        let address = {
            let mut inner = inner.write().unwrap();

            match inner.state {
                ConnectionState::Connecting => return,
                ConnectionState::Connected(_) if inner.generation != failed_generation => return,
                _ => (),
            }

            inner.state = ConnectionState::Connecting;
            inner.address.clone()
        };

        let connection = std::panic::catch_unwind(|| Connection::connect(&address));

        let mut inner = inner.write().unwrap();
        match connection {
            Ok(connection) => {
                inner.generation += 1;
                inner.state = ConnectionState::Connected(connection);
            }
            Err(_) => {
                error!("Panicked while connecting, the connection will be retried");
                inner.state = ConnectionState::Disconnected;
            }
        }
    }

    #[instrument(level = "debug")]
    pub fn new_component_id() -> Result<u8> {
        let manager = MANAGER.lock().unwrap();

        let ids_range = crate::cli::manager::mavlink_camera_component_id_range();
        let mut vector = manager.ids.write().unwrap();

        // Find the closest ID available
        let id = ids_range
            .into_iter()
            .find(|id| !vector.contains(id))
            .context("All available MAVLink componenet IDs are being used ({ids_range:?}). Relaunch MCM with a larger mavlink_camera_component_id_range")?;

        vector.push(id);
        Ok(id)
    }

    #[instrument(level = "debug")]
    pub fn drop_id(id: u8) {
        let manager = MANAGER.lock().unwrap();
        let mut vector = manager.ids.write().unwrap();

        if let Some(position) = vector.iter().position(|&vec_id| vec_id == id) {
            vector.remove(position);
        } else {
            error!("Id not found");
        }
    }

    #[instrument(level = "debug")]
    pub fn get_sender() -> broadcast::Sender<Message> {
        let manager = MANAGER.lock().unwrap();

        let connection = manager.connection.read().unwrap();

        connection.sender.clone()
    }
}

impl Connection {
    #[instrument(level = "debug")]
    fn connect(address: &str) -> Box<dyn MavConnection<MavMessage> + Sync + Send> {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));

            debug!("Connecting... ({address})");

            match mavlink::connect(address) {
                Ok(connection) => {
                    info!("Successfully connected");
                    return connection;
                }
                Err(error) => {
                    error!("Failed to connect, trying again in one second. Reason: {error:?}.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reconnection has to rebind the same address. Dropping the old connection only after
    /// connecting used to leave the socket bound, making every retry fail with EADDRINUSE.
    #[test]
    fn reconnect_rebinds_the_same_address() {
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let inner = Arc::new(RwLock::new(Connection {
            address: format!("udpin:127.0.0.1:{port}"),
            state: ConnectionState::Disconnected,
            generation: 0,
            sender: broadcast::channel(1).0,
        }));

        // Reconnect on another thread, because a failure retries forever instead of returning
        let (done_sender, done) = std::sync::mpsc::channel();
        let connecting = inner.clone();
        std::thread::spawn(move || {
            Manager::reconnect(&connecting, 0);
            Manager::reconnect(&connecting, 1);
            let _ = done_sender.send(());
        });

        done.recv_timeout(std::time::Duration::from_secs(30))
            .expect("Reconnection should rebind the address instead of retrying forever");

        assert!(matches!(
            inner.read().unwrap().state,
            ConnectionState::Connected(_)
        ));
        assert_eq!(inner.read().unwrap().generation, 2);

        Manager::reconnect(&inner, 1);
        assert_eq!(inner.read().unwrap().generation, 2);
    }

    #[test]
    fn sender_keeps_the_message_across_a_reconnection() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .unwrap();

        let (sender, _receiver) = broadcast::channel(1);
        let inner = Arc::new(RwLock::new(Connection {
            address: format!("udpout:{}", socket.local_addr().unwrap()),
            state: ConnectionState::Disconnected,
            generation: 0,
            sender: sender.clone(),
        }));

        let looping = inner.clone();
        std::thread::spawn(move || Manager::sender_loop(looping));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while sender.receiver_count() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "The loop never subscribed"
            );
            std::thread::yield_now();
        }
        sender
            .send(Message::ToBeSent((
                MavHeader::default(),
                MavMessage::HEARTBEAT(Default::default()),
            )))
            .unwrap();

        socket
            .recv(&mut [0; 280])
            .expect("The message should be sent after reconnecting");
    }
}
