use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Result};
use mavlink::{common::MavMessage, MavConnection, MavHeader};
use tokio::sync::mpsc;

pub struct RecordingClient {
    connection: Arc<Box<dyn MavConnection<MavMessage> + Sync + Send>>,
    receiver: mpsc::UnboundedReceiver<(MavHeader, MavMessage)>,
    _recv_thread_stop: Arc<AtomicBool>,
    source_system: u8,
    source_component: u8,
    target_system: u8,
    target_component: u8,
}

#[derive(Debug, Clone)]
pub struct CaptureStatus {
    pub video_status: u8,
    pub is_recording: bool,
    pub recording_time_ms: u32,
    pub available_capacity_mb: f32,
}

#[derive(Debug, Clone)]
pub struct CameraInformation {
    pub vendor_name: String,
    pub model_name: String,
    pub resolution_h: u16,
    pub resolution_v: u16,
    pub firmware_version: u32,
}

#[derive(Debug, Clone)]
pub struct StorageInformation {
    pub total_capacity_mb: f32,
    pub used_capacity_mb: f32,
    pub available_capacity_mb: f32,
}

impl RecordingClient {
    /// Connect to a MAVLink endpoint.
    ///
    /// `address` uses mavlink crate connection strings, e.g. `"tcpout:localhost:14550"`.
    /// `target_system` is the MAVLink system ID of the camera manager (typically 1).
    pub fn new(address: &str, target_system: u8) -> Result<Self> {
        let connection: Arc<Box<dyn MavConnection<MavMessage> + Sync + Send>> =
            Arc::new(mavlink::connect(address)?);

        let (sender, receiver) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));

        let conn_clone = connection.clone();
        let stop_clone = stop.clone();
        std::thread::Builder::new()
            .name("RecordingClientRecv".into())
            .spawn(move || recv_loop(conn_clone, sender, stop_clone))
            .expect("Failed to spawn RecordingClient recv thread");

        Ok(Self {
            connection,
            receiver,
            _recv_thread_stop: stop,
            source_system: 255,
            source_component: mavlink::common::MavComponent::MAV_COMP_ID_MISSIONPLANNER as u8,
            target_system,
            target_component: 0,
        })
    }

    pub fn send_heartbeat(&self) {
        let header = self.command_header();
        let message = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_GCS,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_INVALID,
            base_mode: mavlink::common::MavModeFlag::empty(),
            system_status: mavlink::common::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 0x03,
        });
        let _ = self.connection.send(&header, &message);
    }

    /// Wait for a `HEARTBEAT` from a camera on the target system.
    /// Returns `(system_id, component_id)` and stores the component for subsequent commands.
    pub async fn wait_for_camera_heartbeat(&mut self, timeout: Duration) -> Result<(u8, u8)> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.send_heartbeat();

            match tokio::time::timeout(Duration::from_secs(1), self.receiver.recv()).await {
                Ok(Some((header, MavMessage::HEARTBEAT(data)))) => {
                    if header.system_id == self.target_system
                        && data.mavtype == mavlink::common::MavType::MAV_TYPE_CAMERA
                    {
                        self.target_component = header.component_id;
                        return Ok((header.system_id, header.component_id));
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(anyhow!("MAVLink receive channel closed")),
                Err(_) => {}
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "Timeout waiting for camera heartbeat after {timeout:?}"
                ));
            }
        }
    }

    /// Send `MAV_CMD_VIDEO_START_CAPTURE` and wait for ACK.
    pub async fn start_recording(&mut self, stream_id: u8, status_frequency_hz: f32) -> Result<()> {
        self.send_command_long(
            mavlink::common::MavCmd::MAV_CMD_VIDEO_START_CAPTURE,
            stream_id as f32,
            status_frequency_hz,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        )
        .await
    }

    /// Send `MAV_CMD_VIDEO_STOP_CAPTURE` and wait for ACK.
    pub async fn stop_recording(&mut self, stream_id: u8) -> Result<()> {
        self.send_command_long(
            mavlink::common::MavCmd::MAV_CMD_VIDEO_STOP_CAPTURE,
            stream_id as f32,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        )
        .await
    }

    /// Send `MAV_CMD_REQUEST_CAMERA_INFORMATION` and wait for the response message.
    pub async fn request_camera_information(
        &mut self,
        timeout: Duration,
    ) -> Result<CameraInformation> {
        self.send_command_long(
            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_INFORMATION,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        )
        .await?;

        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, self.receiver.recv()).await {
                Ok(Some((_, MavMessage::CAMERA_INFORMATION(data)))) => {
                    return Ok(CameraInformation {
                        vendor_name: String::from_utf8_lossy(&data.vendor_name)
                            .trim_end_matches('\0')
                            .to_string(),
                        model_name: String::from_utf8_lossy(&data.model_name)
                            .trim_end_matches('\0')
                            .to_string(),
                        resolution_h: data.resolution_h,
                        resolution_v: data.resolution_v,
                        firmware_version: data.firmware_version,
                    });
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(anyhow!("MAVLink receive channel closed")),
                Err(_) => break,
            }
        }
        Err(anyhow!(
            "Timeout waiting for CAMERA_INFORMATION after {timeout:?}"
        ))
    }

    /// Send `MAV_CMD_REQUEST_STORAGE_INFORMATION` and wait for the response message.
    pub async fn request_storage_information(
        &mut self,
        timeout: Duration,
    ) -> Result<StorageInformation> {
        self.send_command_long(
            mavlink::common::MavCmd::MAV_CMD_REQUEST_STORAGE_INFORMATION,
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        )
        .await?;

        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, self.receiver.recv()).await {
                Ok(Some((_, MavMessage::STORAGE_INFORMATION(data)))) => {
                    return Ok(StorageInformation {
                        total_capacity_mb: data.total_capacity,
                        used_capacity_mb: data.used_capacity,
                        available_capacity_mb: data.available_capacity,
                    });
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(anyhow!("MAVLink receive channel closed")),
                Err(_) => break,
            }
        }
        Err(anyhow!(
            "Timeout waiting for STORAGE_INFORMATION after {timeout:?}"
        ))
    }

    /// Request `CAMERA_CAPTURE_STATUS` and wait for the response message.
    pub async fn request_capture_status(&mut self, timeout: Duration) -> Result<CaptureStatus> {
        let header = self.command_header();
        let message = MavMessage::COMMAND_LONG(mavlink::common::COMMAND_LONG_DATA {
            target_system: self.target_system,
            target_component: self.target_component,
            command: mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS,
            confirmation: 0,
            param1: 1.0,
            param2: 0.0,
            param3: 0.0,
            param4: 0.0,
            param5: 0.0,
            param6: 0.0,
            param7: 0.0,
        });
        self.connection
            .send(&header, &message)
            .map_err(|error| anyhow!("Failed to send command: {error}"))?;

        let deadline = tokio::time::Instant::now() + timeout;
        let mut got_ack = false;
        let mut capture_status: Option<CaptureStatus> = None;

        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, self.receiver.recv()).await {
                Ok(Some((_, MavMessage::COMMAND_ACK(ack)))) => {
                    if ack.command == mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS
                    {
                        if ack.result != mavlink::common::MavResult::MAV_RESULT_ACCEPTED {
                            return Err(anyhow!(
                                "CAMERA_CAPTURE_STATUS request rejected: {:?}",
                                ack.result
                            ));
                        }
                        got_ack = true;
                        if capture_status.is_some() {
                            return Ok(capture_status.unwrap());
                        }
                    }
                }
                Ok(Some((_, MavMessage::CAMERA_CAPTURE_STATUS(data)))) => {
                    capture_status = Some(CaptureStatus {
                        video_status: data.video_status,
                        is_recording: data.video_status == 1,
                        recording_time_ms: data.recording_time_ms,
                        available_capacity_mb: data.available_capacity,
                    });
                    if got_ack {
                        return Ok(capture_status.unwrap());
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(anyhow!("MAVLink receive channel closed")),
                Err(_) => break,
            }
        }

        if let Some(status) = capture_status {
            return Ok(status);
        }

        Err(anyhow!(
            "Timeout waiting for CAMERA_CAPTURE_STATUS after {timeout:?}"
        ))
    }

    /// Wait for unsolicited `CAMERA_CAPTURE_STATUS` messages that arrive from
    /// the periodic streaming task started via `status_frequency_hz`.
    pub async fn recv_capture_status(&mut self, timeout: Duration) -> Result<CaptureStatus> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, self.receiver.recv()).await {
                Ok(Some((_, MavMessage::CAMERA_CAPTURE_STATUS(data)))) => {
                    return Ok(CaptureStatus {
                        video_status: data.video_status,
                        is_recording: data.video_status == 1,
                        recording_time_ms: data.recording_time_ms,
                        available_capacity_mb: data.available_capacity,
                    });
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(anyhow!("MAVLink receive channel closed")),
                Err(_) => break,
            }
        }
        Err(anyhow!(
            "No CAMERA_CAPTURE_STATUS received within {timeout:?}"
        ))
    }

    async fn send_command_long(
        &mut self,
        command: mavlink::common::MavCmd,
        param1: f32,
        param2: f32,
        param3: f32,
        param4: f32,
        param5: f32,
        param6: f32,
        param7: f32,
    ) -> Result<()> {
        let header = self.command_header();
        let message = MavMessage::COMMAND_LONG(mavlink::common::COMMAND_LONG_DATA {
            target_system: self.target_system,
            target_component: self.target_component,
            command,
            confirmation: 0,
            param1,
            param2,
            param3,
            param4,
            param5,
            param6,
            param7,
        });
        self.connection
            .send(&header, &message)
            .map_err(|error| anyhow!("Failed to send command: {error}"))?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, self.receiver.recv()).await {
                Ok(Some((_, MavMessage::COMMAND_ACK(ack)))) if ack.command == command => {
                    return if ack.result == mavlink::common::MavResult::MAV_RESULT_ACCEPTED {
                        Ok(())
                    } else {
                        Err(anyhow!("Command {command:?} rejected: {:?}", ack.result))
                    };
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(anyhow!("MAVLink receive channel closed")),
                Err(_) => break,
            }
        }

        Err(anyhow!("Timeout waiting for ACK of {command:?}"))
    }

    fn command_header(&self) -> MavHeader {
        MavHeader {
            system_id: self.source_system,
            component_id: self.source_component,
            sequence: 0,
        }
    }
}

impl Drop for RecordingClient {
    fn drop(&mut self) {
        self._recv_thread_stop.store(true, Ordering::Relaxed);
    }
}

fn recv_loop(
    connection: Arc<Box<dyn MavConnection<MavMessage> + Sync + Send>>,
    sender: mpsc::UnboundedSender<(MavHeader, MavMessage)>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        match connection.recv() {
            Ok((header, message)) => {
                if sender.send((header, message)).is_err() {
                    break;
                }
            }
            Err(mavlink::error::MessageReadError::Io(ref error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
