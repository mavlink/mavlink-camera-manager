use std::sync::Arc;

use crate::{
    cli, mavlink::mavlink_camera_component::MavlinkCameraComponent,
    network::utils::get_visible_qgc_address, video::types::VideoSourceType,
    video_stream::types::VideoAndStreamInformation,
};

use anyhow::{anyhow, Context, Result};
use mavlink::{common::MavMessage, MavHeader};
use tokio::sync::broadcast;
use tracing::*;
use url::Url;

use super::manager::Message;
use super::utils::*;

#[derive(Debug)]
pub struct MavlinkCameraHandle {
    inner: Arc<MavlinkCamera>,
    _runtime: tokio::runtime::Runtime,
    heartbeat_handle: tokio::task::JoinHandle<()>,
    messages_handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct MavlinkCamera {
    component: MavlinkCameraComponent,
    mavlink_stream_type: mavlink::common::VideoStreamType,
    video_stream_uri: Url,
    video_stream_name: String,
    video_source_type: VideoSourceType,
}

impl MavlinkCameraHandle {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let inner = Arc::new(MavlinkCamera::try_new(video_and_stream_information)?);

        let sender = crate::mavlink::manager::Manager::get_sender();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .on_thread_start(|| debug!("Thread started"))
            .on_thread_stop(|| debug!("Thread stopped"))
            .thread_name_fn(|| {
                static ATOMIC_ID: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let id = ATOMIC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("MavlinkCamera-{id}")
            })
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed building a new tokio runtime");

        let heartbeat_handle =
            runtime.spawn(MavlinkCamera::heartbeat_loop(inner.clone(), sender.clone()));
        let messages_handle =
            runtime.spawn(MavlinkCamera::messages_loop(inner.clone(), sender.clone()));

        Ok(Self {
            inner,
            _runtime: runtime,
            heartbeat_handle,
            messages_handle,
        })
    }
}

impl MavlinkCamera {
    #[instrument(level = "debug")]
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let video_stream_uri = video_and_stream_information
            .stream_information
            .endpoints
            .first()
            .context("Empty URI list")?
            .to_owned();

        let mavlink_stream_type = match video_stream_uri.scheme() {
            "rtsp" => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTSP,
            "udp" => mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTPUDP,
            unsupported => {
                return Err(anyhow!(
                    "Scheme {unsupported:#?} is not supported for a Mavlink Camera."
                ));
            }
        };

        let video_stream_name = video_and_stream_information.name.clone();

        let video_source_type = video_and_stream_information.video_source.clone();

        let component_id = super::manager::Manager::new_component_id();
        let component =
            MavlinkCameraComponent::try_new(video_and_stream_information, component_id)?;

        let this = Self {
            component,
            mavlink_stream_type,
            video_stream_uri,
            video_stream_name,
            video_source_type,
        };

        debug!("Starting new MAVLink camera: {this:#?}");

        Ok(this)
    }

    #[instrument(level = "debug")]
    pub fn cam_definition_uri(&self) -> Option<Url> {
        // Get the current remotely accessible link (from default interface)
        // to our camera XML file.
        // This can't be a parameter because the default network route might
        // change between the time of the MavlinkCamera creation
        // and the time MAVLink connection is negotiated with the other MAVLink
        // systems.
        let visible_qgc_ip_address = get_visible_qgc_address();
        let address = cli::manager::server_address();
        let server_port = address.split(':').collect::<Vec<&str>>()[1];
        let video_source_path = self.video_source_type.inner().source_string();
        Url::parse(&format!(
            "http://{visible_qgc_ip_address}:{server_port}/xml?file={video_source_path}"
        ))
        .ok()
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip_all, fields(component_id = camera.component.component_id))]
    pub async fn heartbeat_loop(camera: Arc<MavlinkCamera>, sender: broadcast::Sender<Message>) {
        let component_id = camera.component.component_id;
        let system_id = camera.component.system_id;

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let header = mavlink::MavHeader {
                system_id,
                component_id,
                ..Default::default()
            };

            let message = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
                custom_mode: 0,
                mavtype: mavlink::common::MavType::MAV_TYPE_CAMERA,
                autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_INVALID,
                base_mode: mavlink::common::MavModeFlag::empty(),
                system_status: mavlink::common::MavState::MAV_STATE_STANDBY,
                mavlink_version: 0x3,
            });

            if let Err(error) = sender.send(Message::ToBeSent((header, message))) {
                error!("Failed to send message: {error:?}");
                continue;
            }

            debug!("Heartbeat sent");
        }
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip_all, fields(component_id = camera.component.component_id))]
    pub async fn messages_loop(camera: Arc<MavlinkCamera>, sender: broadcast::Sender<Message>) {
        let mut receiver = sender.subscribe();

        loop {
            let (header, message) = match receiver.recv().await {
                Ok(Message::Received(message)) => message,
                Err(broadcast::error::RecvError::Closed) => {
                    unreachable!(
                        "Closed channel: This should never happen, this channel is static!"
                    );
                }
                Ok(Message::ToBeSent(_)) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
            };

            trace!("Message received: {header:?}, {message:?}");

            tokio::spawn(Self::handle_message(
                camera.clone(),
                sender.clone(),
                header,
                message,
            ));
        }
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip(sender, camera), fields(component_id = camera.component.component_id))]
    async fn handle_message(
        camera: Arc<MavlinkCamera>,
        sender: broadcast::Sender<Message>,
        header: MavHeader,
        message: MavMessage,
    ) {
        match &message {
            MavMessage::COMMAND_LONG(data) => {
                debug!("Received message");
                Self::handle_command_long(&camera, sender, &header, data).await;
            }
            MavMessage::PARAM_EXT_SET(data) => {
                debug!("Received message");
                Self::handle_param_ext_set(&camera, sender, &header, data).await;
            }
            MavMessage::PARAM_EXT_REQUEST_READ(data) => {
                debug!("Received message");
                Self::handle_param_ext_request_read(&camera, sender, &header, data).await;
            }
            MavMessage::PARAM_EXT_REQUEST_LIST(data) => {
                debug!("Received message");
                Self::handle_param_ext_request_list(&camera, sender, &header, data).await;
            }
            MavMessage::HEARTBEAT(_data) => {
                // We receive a bunch of heartbeat messages, we can ignore it, but as it can be useful for debugging.
                trace!("Received heartbeat");
            }
            _ => {
                // Any other message that is not a heartbeat or command_long
                trace!("Received an unsupported message");
            }
        }
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip(sender, camera), fields(component_id = camera.component.component_id))]
    async fn handle_command_long(
        camera: &MavlinkCamera,
        sender: broadcast::Sender<Message>,
        their_header: &MavHeader,
        data: &mavlink::common::COMMAND_LONG_DATA,
    ) {
        #[instrument(level = "debug", skip(sender))]
        fn send_ack(
            sender: &broadcast::Sender<Message>,
            our_header: mavlink::MavHeader,
            their_header: &mavlink::MavHeader,
            command: mavlink::common::MavCmd,
            result: mavlink::common::MavResult,
        ) {
            if let Err(error) = sender.send(Message::ToBeSent((
                our_header,
                MavMessage::COMMAND_ACK(mavlink::common::COMMAND_ACK_DATA {
                    command,
                    result,
                    target_system: their_header.system_id,
                    target_component: their_header.component_id,
                    ..Default::default()
                }),
            ))) {
                warn!("Failed to send message: {error:?}");
            }
        }

        let our_header = camera.component.header(None);

        if data.target_system != our_header.system_id
            || data.target_component != our_header.component_id
        {
            trace!("Ignoring {data:?}, wrong command id or system id");
            return;
        }

        match data.command {
            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_INFORMATION => {
                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                let message =
                    MavMessage::CAMERA_INFORMATION(mavlink::common::CAMERA_INFORMATION_DATA {
                        time_boot_ms: super::sys_info::sys_info().time_boot_ms,
                        firmware_version: 0,
                        focal_length: 0.0,
                        sensor_size_h: 0.0,
                        sensor_size_v: 0.0,
                        flags: mavlink::common::CameraCapFlags::CAMERA_CAP_FLAGS_HAS_VIDEO_STREAM,
                        resolution_h: camera.component.resolution_h,
                        resolution_v: camera.component.resolution_v,
                        cam_definition_version: 0,
                        vendor_name: from_string_to_u8_array_with_size_32(
                            &camera.component.vendor_name,
                        ),
                        model_name: from_string_to_u8_array_with_size_32(
                            &camera.component.vendor_name,
                        ),

                        lens_id: 0,
                        cam_definition_uri:
                            from_string_to_vec_char_with_defined_size_and_null_terminator(
                                camera.cam_definition_uri().unwrap().as_str(),
                                140,
                            ),
                    });

                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            }
            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_SETTINGS => {
                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                let message = MavMessage::CAMERA_SETTINGS(mavlink::common::CAMERA_SETTINGS_DATA {
                    time_boot_ms: super::sys_info::sys_info().time_boot_ms,
                    zoomLevel: 0.0,
                    focusLevel: 0.0,
                    mode_id: mavlink::common::CameraMode::CAMERA_MODE_VIDEO,
                });

                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            }
            mavlink::common::MavCmd::MAV_CMD_REQUEST_STORAGE_INFORMATION => {
                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                let sys_info = super::sys_info::sys_info();
                let message =
                    MavMessage::STORAGE_INFORMATION(mavlink::common::STORAGE_INFORMATION_DATA {
                        time_boot_ms: sys_info.time_boot_ms,
                        total_capacity: sys_info.total_capacity,
                        used_capacity: sys_info.used_capacity,
                        available_capacity: sys_info.available_capacity,
                        read_speed: 1000.0,
                        write_speed: 1000.0,
                        storage_id: 0,
                        storage_count: 0,
                        status: mavlink::common::StorageStatus::STORAGE_STATUS_READY,
                    });

                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            }
            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS => {
                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                let sys_info = super::sys_info::sys_info();
                let message = MavMessage::CAMERA_CAPTURE_STATUS(
                    mavlink::common::CAMERA_CAPTURE_STATUS_DATA {
                        time_boot_ms: sys_info.time_boot_ms,
                        image_interval: 0.0,
                        recording_time_ms: 0,
                        available_capacity: sys_info.available_capacity,
                        image_status: 0,
                        video_status: 0,
                        image_count: 0,
                    },
                );

                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            }
            mavlink::common::MavCmd::MAV_CMD_REQUEST_VIDEO_STREAM_INFORMATION => {
                const ALL_CAMERAS: u8 = 0u8;
                if data.param2 != (camera.component.stream_id as f32)
                    && data.param2 != (ALL_CAMERAS as f32)
                {
                    warn!("Unknown stream id: {:#?}.", data.param2);

                    let result = mavlink::common::MavResult::MAV_RESULT_UNSUPPORTED;
                    send_ack(&sender, our_header, their_header, data.command, result);

                    return;
                }

                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                // The only important information here is the mavtype and uri variables, everything else can be fake
                let message = MavMessage::VIDEO_STREAM_INFORMATION(
                    mavlink::common::VIDEO_STREAM_INFORMATION_DATA {
                        framerate: camera.component.framerate,
                        bitrate: camera.component.bitrate,
                        flags: get_stream_status_flag(&camera.component),
                        resolution_h: camera.component.resolution_h,
                        resolution_v: camera.component.resolution_v,
                        rotation: camera.component.rotation,
                        hfov: camera.component.hfov,
                        stream_id: camera.component.stream_id,
                        count: 0,
                        mavtype: camera.mavlink_stream_type,
                        name: from_string_to_char_array_with_size_32(&camera.video_stream_name),
                        uri: from_string_to_vec_char_with_defined_size_and_null_terminator(
                            camera.video_stream_uri.as_ref(),
                            140,
                        ),
                    },
                );

                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            }
            mavlink::common::MavCmd::MAV_CMD_RESET_CAMERA_SETTINGS => {
                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                let source_string = camera.video_source_type.inner().source_string();
                let result = match crate::video::video_source::reset_controls(source_string) {
                    Ok(_) => mavlink::common::MavResult::MAV_RESULT_ACCEPTED,
                    Err(error) => {
                        error!("Failed to reset {source_string:?} controls with its default values as {:#?}:{:#?}. Reason: {error:?}", our_header.system_id, our_header.component_id);
                        mavlink::common::MavResult::MAV_RESULT_DENIED
                    }
                };

                send_ack(&sender, our_header, their_header, data.command, result);
            }
            mavlink::common::MavCmd::MAV_CMD_REQUEST_VIDEO_STREAM_STATUS => {
                let result = mavlink::common::MavResult::MAV_RESULT_ACCEPTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                // The only important information here is the mavtype and uri variables, everything else can be fake
                let message =
                    MavMessage::VIDEO_STREAM_STATUS(mavlink::common::VIDEO_STREAM_STATUS_DATA {
                        framerate: camera.component.framerate,
                        bitrate: camera.component.bitrate,
                        flags: get_stream_status_flag(&camera.component),
                        resolution_h: camera.component.resolution_h,
                        resolution_v: camera.component.resolution_v,
                        rotation: camera.component.rotation,
                        hfov: camera.component.hfov,
                        stream_id: camera.component.stream_id,
                    });

                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            }
            mavlink::common::MavCmd::MAV_CMD_REQUEST_MESSAGE => {
                let result = mavlink::common::MavResult::MAV_RESULT_UNSUPPORTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                warn!("MAVLink message \"MAV_CMD_REQUEST_MESSAGE\" is not supported yet, please report this issue so we can prioritize it. Meanwhile, you can use the original definitions for the MAVLink Camera Protocol. Read more in: https://mavlink.io/en/services/camera.html#migration-notes-for-gcs--mavlink-sdks");
            }
            message => {
                let result = mavlink::common::MavResult::MAV_RESULT_UNSUPPORTED;
                send_ack(&sender, our_header, their_header, data.command, result);

                trace!("Ignoring unknown message received: {message:?}")
            }
        }
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip(sender, camera), fields(component_id = camera.component.component_id))]
    async fn handle_param_ext_set(
        camera: &MavlinkCamera,
        sender: broadcast::Sender<Message>,
        header: &MavHeader,
        data: &mavlink::common::PARAM_EXT_SET_DATA,
    ) {
        #[instrument(level = "debug", skip(sender))]
        fn send_ack(
            sender: &broadcast::Sender<Message>,
            our_header: mavlink::MavHeader,
            data: &mavlink::common::PARAM_EXT_SET_DATA,
            result: mavlink::common::ParamAck,
        ) {
            if let Err(error) = sender.send(Message::ToBeSent((
                our_header,
                MavMessage::PARAM_EXT_ACK(mavlink::common::PARAM_EXT_ACK_DATA {
                    param_id: data.param_id,
                    param_value: data.param_value.clone(),
                    param_type: data.param_type,
                    param_result: result,
                }),
            ))) {
                warn!("Failed to send message: {error:?}");
            }
        }

        let our_header = camera.component.header(None);

        if data.target_system != our_header.system_id
            || data.target_component != our_header.component_id
        {
            trace!("Ignoring {data:?}, wrong command id or system id");
            return;
        }

        let control_id = control_id_from_param_id(&data.param_id);
        let control_value = control_value_from_param_value(&data.param_value, &data.param_type);
        let (Some(control_id), Some(control_value)) = (control_id, control_value) else {
            let result = mavlink::common::ParamAck::PARAM_ACK_VALUE_UNSUPPORTED;
            send_ack(&sender, our_header, data, result);

            return;
        };

        let result = match camera
            .video_source_type
            .inner()
            .set_control_by_id(control_id, control_value)
        {
            Ok(_) => mavlink::common::ParamAck::PARAM_ACK_ACCEPTED,
            Err(error) => {
                error!("Failed to set parameter {control_id:?} with value {control_value:?} for {:#?}. Reason: {error:?}", our_header.component_id);
                mavlink::common::ParamAck::PARAM_ACK_FAILED
            }
        };

        send_ack(&sender, our_header, data, result);
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip(sender, camera), fields(component_id = camera.component.component_id))]
    async fn handle_param_ext_request_read(
        camera: &MavlinkCamera,
        sender: broadcast::Sender<Message>,
        header: &MavHeader,
        data: &mavlink::common::PARAM_EXT_REQUEST_READ_DATA,
    ) {
        let our_header = camera.component.header(None);

        if data.target_system != our_header.system_id
            || data.target_component != our_header.component_id
        {
            trace!("Ignoring {data:?}, wrong command id or system id");
            return;
        }

        let controls = camera.video_source_type.inner().controls();
        let Some((param_index, control_id)) = get_param_index_and_control_id(data, &controls)
        else {
            return;
        };

        let param_id = param_id_from_control_id(control_id);
        let control_value = match camera
            .video_source_type
            .inner()
            .control_value_by_id(control_id)
        {
            Ok(value) => value,
            Err(error) => {
                error!("Failed to get parameter {control_id:?}: {error:?}");
                return;
            }
        };

        let param_value = param_value_from_control_value(control_value, 128);

        let our_header = camera.component.header(None);
        let message = MavMessage::PARAM_EXT_VALUE(mavlink::common::PARAM_EXT_VALUE_DATA {
            param_count: 1,
            param_index,
            param_id,
            param_value,
            param_type: mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT64,
        });
        if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
            warn!("Failed to send message: {error:?}");
        }
    }

    #[instrument(level = "trace", skip(sender))]
    #[instrument(level = "debug", skip(sender, camera), fields(component_id = camera.component.component_id))]
    async fn handle_param_ext_request_list(
        camera: &MavlinkCamera,
        sender: broadcast::Sender<Message>,
        header: &MavHeader,
        data: &mavlink::common::PARAM_EXT_REQUEST_LIST_DATA,
    ) {
        let our_header = camera.component.header(None);

        if data.target_system != our_header.system_id
            || data.target_component != our_header.component_id
        {
            trace!("Ignoring {data:?}, wrong command id or system id");
            return;
        }

        let controls = camera.video_source_type.inner().controls();

        controls
            .iter()
            .enumerate()
            .for_each(|(param_index, control)| {
                let param_id = param_id_from_control_id(control.id);
                let control_value = match camera
                    .video_source_type
                    .inner()
                    .control_value_by_id(control.id)
                {
                    Ok(value) => value,
                    Err(error) => {
                        error!("Failed to get parameter {:?}: {error:?}", control.id);
                        return;
                    }
                };

                let param_value = param_value_from_control_value(control_value, 128);

                let our_header = camera.component.header(None);
                let message = MavMessage::PARAM_EXT_VALUE(mavlink::common::PARAM_EXT_VALUE_DATA {
                    param_count: controls.len() as u16,
                    param_index: param_index as u16,
                    param_id,
                    param_value,
                    param_type: mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT64,
                });
                if let Err(error) = sender.send(Message::ToBeSent((our_header, message))) {
                    warn!("Failed to send message: {error:?}");
                }
            });
    }
}

impl Drop for MavlinkCameraHandle {
    fn drop(&mut self) {
        self.heartbeat_handle.abort();
        self.messages_handle.abort();
        super::manager::Manager::drop_id(self.inner.component.component_id)
    }
}
