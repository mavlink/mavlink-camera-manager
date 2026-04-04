// As defined, our Event/Log protocol is defined as the following examples:
//
// Example for LogMessage :
// {
//     "name": "c60560441e4cea1e4271b73947e0cb12/mavlink-camera-manager",
//     "message": "This is a log message"
// }
//
// Example for EventMessage :
// {
//     "name": "c60560441e4cea1e4271b73947e0cb12/mavlink-camera-manager",
//     "message": {
//         "type": "start",
//         "runtime_pwd": "/root",
//         "runtime_user": "root",
//         "runtime_cmdline": [
//           "mavlink-camera-manager",
//           "--mavlink",
//           "tcpout:127.0.0.1:5777",
//           "--settings-file",
//           "~/.config/mavlink-camera-manager/settings.json",
//           "--reset",
//           "--rest-server",
//           "0.0.0.0:6020",
//           "--stun-server",
//           "stun://0.0.0.0:3478",
//           "--signalling-server",
//           "ws://0.0.0.0:6021",
//           "--verbose",
//           "--log-path",
//           "/var/logs/blueos/services/mavlink-camera-manager",
//           "--mavlink-system-id",
//           "1",
//           "--mavlink-camera-component-id-range",
//           "100..=105",
//           "--zenoh"
//         ],
//         "app_name": "mavlink-camera-manager",
//         "app_version": "t3.22.3-8-g7992334c-dirty",
//     }
// }

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "message", rename_all = "snake_case")]
pub enum Message {
    Log(LogMessage),
    Event(EventMessage),
}

#[derive(Serialize, Deserialize)]
pub struct LogMessage {
    pub name: String,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventMessage {
    Start(StartEvent),
    Settings(SettingsEvent),
    Running(RunningEvent),
}

#[derive(Debug, Serialize, Deserialize)]
/// Emitted once the service is started, as early as possible.
pub struct StartEvent {
    pub runtime_pwd: String,
    pub runtime_user: String,
    pub runtime_cmdline: Vec<String>, // std::env::args().collect()
    pub app_name: String,
    pub app_version: String, // git describe --tags --dirty --always
}

#[derive(Serialize, Deserialize)]
/// Emitted whenever the saved settings change. To prevent bursts, send only when the file content is changed.
pub struct SettingsEvent {
    pub settings: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
/// Emitted once after everything is initialized.
pub struct RunningEvent;
