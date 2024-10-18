use std::marker::Send;
use std::sync::{Arc, Mutex, RwLock};

use mavlink::common::MavMessage;
use mavlink::{MavConnection, MavHeader};

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
    connection: Option<Box<dyn MavConnection<MavMessage> + Sync + Send>>,
    sender: broadcast::Sender<Message>,
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
                connection: None,
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
        loop {
            loop {
                let Ok(inner_guard) = inner.read() else {
                    break; // Break to trigger reconnection
                };
                let Some(mavlink) = inner_guard.connection.as_deref() else {
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

            // Reconnects
            {
                let mut inner = inner.write().unwrap();
                let address = inner.address.clone();
                inner.connection.replace(Connection::connect(&address));
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    #[instrument(level = "debug", skip(inner))]
    fn sender_loop(inner: Arc<RwLock<Connection>>) {
        let mut receiver = { inner.read().unwrap().sender.subscribe() };

        loop {
            loop {
                // Receive answer from the cameras
                let (header, message) = match receiver.blocking_recv() {
                    Ok(Message::ToBeSent(message)) => message,
                    Ok(Message::Received(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        unreachable!(
                            "Closed channel: This should never happen, this channel is static!"
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(samples)) => {
                        warn!("Channel is lagged behind by {samples} messages. Expect degraded performance on the mavlink responsiviness.");
                        continue;
                    }
                };

                let Ok(inner_guard) = inner.read() else {
                    break; // Break to trigger reconnection
                };
                let Some(mavlink) = inner_guard.connection.as_deref() else {
                    break; // Break to trigger reconnection
                };

                // Send the response from the cameras to the Mavlink network
                if let Err(error) = mavlink.send(&header, &message) {
                    error!("Failed sending message to Mavlink Connection: {error:?}");

                    break; // Break to trigger reconnection
                }

                debug!("Message sent: {header:?}, {message:?}");
            }

            // Reconnects
            {
                let mut inner = inner.write().unwrap();
                let address = inner.address.clone();
                inner.connection.replace(Connection::connect(&address));
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    #[instrument(level = "debug")]
    pub fn new_component_id() -> u8 {
        let manager = MANAGER.lock().unwrap();

        let mut id = mavlink::common::MavComponent::MAV_COMP_ID_CAMERA as u8;
        let mut vector = manager.ids.write().unwrap();

        // Find the closest ID available
        while vector.contains(&id) {
            id += 1;
        }

        vector.push(id);
        id
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

            debug!("Connecting...");

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
