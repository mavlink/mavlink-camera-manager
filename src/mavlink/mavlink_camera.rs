use crate::cli;
use crate::network;
use crate::settings;
use crate::video::types::VideoSourceType;

use log::*;
use simple_error::SimpleError;
use url::Url;

use std::convert::TryInto;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

lazy_static! {
    static ref ID_CONTROL: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct MavlinkCameraComponent {
    // MAVLink specific information
    system_id: u8,
    component_id: u8,

    vendor_name: String,
    model_name: String,
    firmware_version: u32,
    resolution_h: f32,
    resolution_v: f32,
}

#[derive(Clone)]
pub struct MavlinkCameraInformation {
    component: MavlinkCameraComponent,
    mavlink_connection_string: String,
    mavlink_stream_type: mavlink::common::VideoStreamType,
    video_stream_uri: Url,
    video_source_type: VideoSourceType,
    thermal: bool,
    vehicle: Arc<Box<dyn mavlink::MavConnection<mavlink::common::MavMessage> + Sync + Send>>,
}

#[derive(Clone, Debug, PartialEq)]
enum ThreadState {
    DEAD,
    RUNNING,
    ZOMBIE,
    RESTART,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MavlinkCameraHandle {
    mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>>,
    thread_state: Arc<Mutex<ThreadState>>,
    heartbeat_thread: std::thread::JoinHandle<()>,
    receive_message_thread: std::thread::JoinHandle<()>,
}

// Debug definition to avoid problems with vehicle type
impl std::fmt::Debug for MavlinkCameraInformation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MavlinkCameraInformation")
            .field("component", &self.component)
            .field("mavlink_connection_string", &self.mavlink_connection_string)
            .field("mavlink_stream_type", &self.mavlink_stream_type)
            .field("video_stream_uri", &self.video_stream_uri)
            .field("video_source_type", &self.video_source_type)
            .field("thermal", &self.thermal)
            .finish()
    }
}

impl Default for MavlinkCameraComponent {
    fn default() -> Self {
        let mut vector = ID_CONTROL.as_ref().lock().unwrap();

        // Find the closer ID available
        let mut id: u8 = 0;
        loop {
            if vector.contains(&id) {
                id += 1;
                continue;
            } else {
                vector.push(id);
                break;
            }
        }

        Self {
            system_id: 1,
            component_id: mavlink::common::MavComponent::MAV_COMP_ID_CAMERA as u8 + id,

            vendor_name: Default::default(),
            model_name: Default::default(),
            firmware_version: 0,
            resolution_h: 0.0,
            resolution_v: 0.0,
        }
    }
}

impl Drop for MavlinkCameraComponent {
    fn drop(&mut self) {
        // Remove id from used ids
        let id = self.component_id - mavlink::common::MavComponent::MAV_COMP_ID_CAMERA as u8;
        let mut vector = ID_CONTROL.as_ref().lock().unwrap();
        let position = vector.iter().position(|&vec_id| vec_id == id).unwrap();
        vector.remove(position);
    }
}

impl MavlinkCameraInformation {
    fn new(
        video_source_type: VideoSourceType,
        mavlink_connection_string: &str,
        video_stream_uri: Url,
        mavlink_stream_type: mavlink::common::VideoStreamType,
        thermal: bool,
    ) -> Self {
        Self {
            component: Default::default(),
            mavlink_connection_string: mavlink_connection_string.into(),
            mavlink_stream_type,
            video_stream_uri,
            video_source_type,
            thermal,
            vehicle: Arc::new(mavlink::connect(&mavlink_connection_string).unwrap()),
        }
    }
}

impl MavlinkCameraHandle {
    pub fn new(
        video_source_type: VideoSourceType,
        endpoint: Url,
        mavlink_stream_type: mavlink::common::VideoStreamType,
        thermal: bool,
    ) -> Self {
        debug!(
            "Starting new MAVLink camera device for: {:#?}, endpoint: {}",
            video_source_type, endpoint
        );

        let mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>> =
            Arc::new(Mutex::new(MavlinkCameraInformation::new(
                video_source_type,
                &settings::manager::mavlink_endpoint(),
                endpoint,
                mavlink_stream_type,
                thermal,
            )));

        let thread_state = Arc::new(Mutex::new(ThreadState::RUNNING));

        let heartbeat_mavlink_information = mavlink_camera_information.clone();
        let receive_message_mavlink_information = mavlink_camera_information.clone();

        let heartbeat_state = thread_state.clone();
        let receive_message_state = thread_state.clone();

        Self {
            mavlink_camera_information: mavlink_camera_information.clone(),
            thread_state: thread_state.clone(),
            heartbeat_thread: std::thread::spawn(move || {
                heartbeat_loop(heartbeat_state.clone(), heartbeat_mavlink_information)
            }),
            receive_message_thread: std::thread::spawn(move || {
                receive_message_loop(
                    receive_message_state.clone(),
                    receive_message_mavlink_information,
                )
            }),
        }
    }
}

impl Drop for MavlinkCameraHandle {
    fn drop(&mut self) {
        let mut state = self.thread_state.as_ref().lock().unwrap();
        *state = ThreadState::DEAD;
    }
}

fn heartbeat_loop(
    atomic_thread_state: Arc<Mutex<ThreadState>>,
    mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>>,
) {
    let mut header = mavlink::MavHeader::default();
    let mavlink_camera_information = mavlink_camera_information.as_ref().lock().unwrap();
    header.system_id = mavlink_camera_information.component.system_id;
    header.component_id = mavlink_camera_information.component.component_id;
    let vehicle = mavlink_camera_information.vehicle.clone();
    drop(mavlink_camera_information);

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));

        let mut heartbeat_state = atomic_thread_state.as_ref().lock().unwrap().clone();
        if heartbeat_state == ThreadState::ZOMBIE {
            continue;
        }
        if heartbeat_state == ThreadState::DEAD {
            break;
        }

        if heartbeat_state == ThreadState::RESTART {
            heartbeat_state = ThreadState::RUNNING;
            drop(heartbeat_state);

            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }

        debug!("sending heartbeat");
        if let Err(error) = vehicle.as_ref().send(&header, &heartbeat_message()) {
            error!("Failed to send heartbeat: {:?}", error);
        }
    }
}

fn receive_message_loop(
    atomic_thread_state: Arc<Mutex<ThreadState>>,
    mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>>,
) {
    let mut header = mavlink::MavHeader::default();
    let information = mavlink_camera_information.as_ref().lock().unwrap();
    header.system_id = information.component.system_id;
    header.component_id = information.component.component_id;

    let vehicle = information.vehicle.clone();
    drop(information);
    let vehicle = vehicle.as_ref();
    loop {
        let loop_state = atomic_thread_state.as_ref().lock().unwrap().clone();
        if loop_state == ThreadState::DEAD {
            break;
        }

        match vehicle.recv() {
            Ok((_header, msg)) => {
                match msg {
                    // Check if there is any camera information request from gcs
                    mavlink::common::MavMessage::COMMAND_LONG(command_long) => {
                        match command_long.command {
                            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_INFORMATION => {
                                debug!("Sending camera_information..");
                                let information =
                                    mavlink_camera_information.as_ref().lock().unwrap();
                                let source_string =
                                    information.video_source_type.inner().source_string();
                                let vendor_name = information.video_source_type.inner().name();

                                let ips = network::utils::get_ipv4_addresses();
                                let visible_qgc_ip_address = &ips.last().unwrap().to_string();
                                let server_port = cli::manager::server_address()
                                    .split(":")
                                    .collect::<Vec<&str>>()[1];

                                if let Err(error) = vehicle.send(
                                    &header,
                                    &camera_information(
                                        vendor_name,
                                        vendor_name,
                                        &format!("{visible_qgc_ip_address}:{server_port}"),
                                        source_string,
                                    ),
                                ) {
                                    warn!("Failed to send camera_information: {:?}", error);
                                }
                            }
                            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_SETTINGS => {
                                debug!("Sending camera_settings..");
                                if let Err(error) = vehicle.send(&header, &camera_settings()) {
                                    warn!("Failed to send camera_settings: {:?}", error);
                                }
                            }
                            mavlink::common::MavCmd::MAV_CMD_REQUEST_STORAGE_INFORMATION => {
                                debug!("Sending camera_storage_information..");
                                if let Err(error) =
                                    vehicle.send(&header, &camera_storage_information())
                                {
                                    warn!("Failed to send camera_storage_information: {:?}", error);
                                }
                            }
                            mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS => {
                                debug!("Sending camera_capture_status..");
                                if let Err(error) = vehicle.send(&header, &camera_capture_status())
                                {
                                    warn!("Failed to send camera_capture_status: {:?}", error);
                                }
                            }
                            mavlink::common::MavCmd::MAV_CMD_REQUEST_VIDEO_STREAM_INFORMATION => {
                                debug!("Sending video_stream_information..");
                                let information =
                                    mavlink_camera_information.as_ref().lock().unwrap();
                                let source_string =
                                    information.video_source_type.inner().source_string();

                                // Remove localhost address with public ip
                                let mut video_url = information.video_stream_uri.clone();
                                if let Ok(address) = std::net::Ipv4Addr::from_str(
                                    video_url.host_str().unwrap_or_default(),
                                ) {
                                    if address == std::net::Ipv4Addr::UNSPECIFIED {
                                        let ips = network::utils::get_ipv4_addresses();
                                        let visible_qgc_ip_address =
                                            &ips.last().unwrap().to_string();
                                        let _ = video_url.set_host(Some(visible_qgc_ip_address));
                                    }
                                }

                                if let Err(error) = vehicle.send(
                                    &header,
                                    &video_stream_information(
                                        &source_string,
                                        &video_url.to_string(),
                                        information.mavlink_stream_type,
                                        information.thermal,
                                    ),
                                ) {
                                    warn!("Failed to send video_stream_information: {:?}", error);
                                }
                            }
                            _ => {
                                let information =
                                    mavlink_camera_information.as_ref().lock().unwrap();
                                warn!(
                                    "Camera: {:#?}, ignoring command: {:#?}",
                                    information, command_long.command
                                );
                            }
                        }
                    }
                    mavlink::common::MavMessage::PARAM_EXT_SET(param_ext_set) => {
                        if param_ext_set.target_system != header.system_id {
                            debug!(
                                "Ignoring PARAM_EXT_SET, wrong system id: {}",
                                param_ext_set.target_system
                            );
                            continue;
                        }

                        if param_ext_set.target_component != header.component_id {
                            debug!(
                                "Ignoring PARAM_EXT_SET, wrong component id: {}",
                                param_ext_set.target_component
                            );
                        }

                        let param_id: String = param_ext_set.param_id.iter().collect();
                        let control_id = param_id.trim_end_matches(char::from(0)).parse::<u64>();

                        let bytes: Vec<u8> =
                            param_ext_set.param_value.iter().map(|c| *c as u8).collect();
                        let control_value = match param_ext_set.param_type {
                            mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_UINT8 => {
                                Ok(u8::from_ne_bytes(bytes[0..1].try_into().unwrap()) as i64)
                            }
                            mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT32 => {
                                Ok(i32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as i64)
                            }
                            mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT64 => {
                                Ok(i64::from_ne_bytes(bytes[0..8].try_into().unwrap()))
                            }
                            something_else => Err(SimpleError::new(format!(
                                "Received parameter of untreatable type: {:#?}",
                                something_else
                            ))),
                        };

                        if let Err(error) = control_id {
                            error!("Failed to parse control id: {:#?}", error);
                            continue;
                        }
                        let _control_id = control_id.unwrap();

                        if let Err(error) = control_value {
                            error!("Failed to parse parameter value: {:#?}", error);
                            continue;
                        }
                        let _control_value = control_value.unwrap();

                        //TODO: Control V4L
                    }

                    //TODO: Handle all necessary QGC messages to setup camera
                    // We receive a bunch of heartbeat messages, we can ignore it
                    mavlink::common::MavMessage::HEARTBEAT(_) => {}
                    // Any other message that is not a heartbeat or command_long
                    _ => {
                        let information = mavlink_camera_information.as_ref().lock().unwrap();
                        debug!("Camera: {:#?}, Ignoring: {:#?}", information, msg);
                    }
                }
            }
            Err(error) => {
                let information = mavlink_camera_information.as_ref().lock().unwrap();
                error!("Camera: {:#?}, Recv error: {:#?}", information, error);
            }
        }
    }
}

#[derive(Debug)]
struct SysInfo {
    time_boot_ms: u32,
    total_capacity: f32,
    used_capacity: f32,
    available_capacity: f32,
}

fn sys_info() -> SysInfo {
    //Both uses KB
    let mut local_total_capacity = 0;
    let mut local_available_capacity = 0;

    match sys_info::disk_info() {
        Ok(disk_info) => {
            local_available_capacity = disk_info.free;
            local_total_capacity = disk_info.total;
        }

        Err(error) => {
            warn!("Failed to fetch disk info: {:#?}", error);
        }
    }

    let boottime_ms = match sys_info::boottime() {
        Ok(bootime) => bootime.tv_usec / 1000,
        Err(error) => {
            warn!("Failed to fetch boottime info: {:#?}", error);
            0
        }
    };

    return SysInfo {
        time_boot_ms: boottime_ms as u32,
        total_capacity: local_total_capacity as f32 / f32::powf(2.0, 10.0),
        used_capacity: ((local_total_capacity - local_available_capacity) as f32)
            / f32::powf(2.0, 10.0),
        available_capacity: local_available_capacity as f32 / f32::powf(2.0, 10.0),
    };
}

//TODO: finish this messages
fn heartbeat_message() -> mavlink::common::MavMessage {
    mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
        custom_mode: 0,
        mavtype: mavlink::common::MavType::MAV_TYPE_CAMERA,
        autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_GENERIC,
        base_mode: mavlink::common::MavModeFlag::empty(),
        system_status: mavlink::common::MavState::MAV_STATE_STANDBY,
        mavlink_version: 0x3,
    })
}

fn camera_information(
    vendor_name: &str,
    model_name: &str,
    http_server_address: &str,
    video_source_path: &str,
) -> mavlink::common::MavMessage {
    // Create a fixed size array with the camera name
    let name_str = String::from(vendor_name);
    let mut vendor_name: [u8; 32] = [0; 32];
    for index in 0..name_str.len() as usize {
        vendor_name[index] = name_str.as_bytes()[index];
    }

    let name_str = String::from(model_name);
    let mut model_name: [u8; 32] = [0; 32];
    for index in 0..name_str.len() as usize {
        model_name[index] = name_str.as_bytes()[index];
    }

    // Send path to our camera configuration file
    let uri = format!(
        r"http://{}/xml?file={}",
        http_server_address, video_source_path
    );

    warn!("URI: {}", uri);
    let uri: Vec<char> = uri.chars().collect();

    let sys_info = sys_info();

    mavlink::common::MavMessage::CAMERA_INFORMATION(mavlink::common::CAMERA_INFORMATION_DATA {
        time_boot_ms: sys_info.time_boot_ms,
        firmware_version: 0,
        focal_length: 0.0,
        sensor_size_h: 0.0,
        sensor_size_v: 0.0,
        flags: mavlink::common::CameraCapFlags::CAMERA_CAP_FLAGS_HAS_VIDEO_STREAM,
        resolution_h: 0,
        resolution_v: 0,
        cam_definition_version: 0,
        vendor_name,
        model_name,
        lens_id: 0,
        cam_definition_uri: uri,
    })
}

fn camera_settings() -> mavlink::common::MavMessage {
    let sys_info = sys_info();

    mavlink::common::MavMessage::CAMERA_SETTINGS(mavlink::common::CAMERA_SETTINGS_DATA {
        time_boot_ms: sys_info.time_boot_ms,
        zoomLevel: 0.0,
        focusLevel: 0.0,
        mode_id: mavlink::common::CameraMode::CAMERA_MODE_VIDEO,
    })
}

fn camera_storage_information() -> mavlink::common::MavMessage {
    let sys_info = sys_info();

    mavlink::common::MavMessage::STORAGE_INFORMATION(mavlink::common::STORAGE_INFORMATION_DATA {
        time_boot_ms: sys_info.time_boot_ms,
        total_capacity: sys_info.total_capacity,
        used_capacity: sys_info.used_capacity,
        available_capacity: sys_info.available_capacity,
        read_speed: 1000.0,
        write_speed: 1000.0,
        storage_id: 0,
        storage_count: 0,
        status: mavlink::common::StorageStatus::STORAGE_STATUS_READY,
    })
}

fn camera_capture_status() -> mavlink::common::MavMessage {
    let sys_info = sys_info();

    mavlink::common::MavMessage::CAMERA_CAPTURE_STATUS(
        mavlink::common::CAMERA_CAPTURE_STATUS_DATA {
            time_boot_ms: sys_info.time_boot_ms,
            image_interval: 0.0,
            recording_time_ms: 0,
            available_capacity: sys_info.available_capacity,
            image_status: 0,
            video_status: 0,
            image_count: 0,
        },
    )
}

fn video_stream_information(
    video_name: &str,
    video_uri: &str,
    mavtype: mavlink::common::VideoStreamType,
    thermal: bool,
) -> mavlink::common::MavMessage {
    let name_str = String::from(video_name);
    let mut name: [char; 32] = ['\0'; 32];
    for index in 0..name_str.len() as u32 {
        name[index as usize] = name_str.as_bytes()[index as usize] as char;
    }

    let uri: Vec<char> = format!("{}\0", video_uri).chars().collect();

    let flags = if thermal {
        mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_THERMAL
    } else {
        mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_RUNNING
    };

    //The only important information here is the mavtype and uri variables, everything else is fake
    mavlink::common::MavMessage::VIDEO_STREAM_INFORMATION(
        mavlink::common::VIDEO_STREAM_INFORMATION_DATA {
            framerate: 30.0,
            bitrate: 1000,
            flags: flags,
            resolution_h: 1000,
            resolution_v: 1000,
            rotation: 0,
            hfov: 0,
            stream_id: 1, // Starts at 1, 0 is for broadcast
            count: 0,
            mavtype: mavtype,
            name,
            uri,
        },
    )
}
