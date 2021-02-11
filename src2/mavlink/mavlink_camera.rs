use std::sync::{Arc, Mutex};

#[derive(Debug)]
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

pub struct MavlinkCameraInformation {
    component: MavlinkCameraComponent,
    mavlink_connection_string: String,
    video_stream_uri: String,
    vehicle: Arc<Mutex<Box<dyn mavlink::MavConnection<mavlink::common::MavMessage> + Sync + Send>>>,
}

#[derive(PartialEq)]
enum ThreadState {
    DEAD,
    RUNNING,
    ZOMBIE,
    RESTART,
}

pub struct MavlinkCameraHandle {
    mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>>,
    heartbeat_state: Arc<Mutex<ThreadState>>,
    heartbeat_thread: std::thread::JoinHandle<()>,
    receive_message_state: Arc<Mutex<ThreadState>>,
    receive_message_thread: std::thread::JoinHandle<()>,
}

impl Default for MavlinkCameraComponent {
    fn default() -> Self {
        Self {
            system_id: 1,
            component_id: mavlink::common::MavComponent::MAV_COMP_ID_CAMERA as u8,

            vendor_name: Default::default(),
            model_name: Default::default(),
            firmware_version: 0,
            resolution_h: 0.0,
            resolution_v: 0.0,
        }
    }
}

impl MavlinkCameraInformation {
    fn new(mavlink_connection_string: &'static str) -> Self {
        Self {
            component: Default::default(),
            mavlink_connection_string: mavlink_connection_string.into(),
            video_stream_uri:
                "rtsp://wowzaec2demo.streamlock.net:554/vod/mp4:BigBuckBunny_115k.mov".into(),
            vehicle: Arc::new(Mutex::new(
                mavlink::connect(&mavlink_connection_string).unwrap(),
            )),
        }
    }
}

impl MavlinkCameraHandle {
    pub fn new() -> Self {
        let mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>> = Arc::new(
            Mutex::new(MavlinkCameraInformation::new("udpout:0.0.0.0:14550")),
        );

        let heartbeat_state = Arc::new(Mutex::new(ThreadState::RUNNING));
        let receive_message_state = Arc::new(Mutex::new(ThreadState::ZOMBIE));

        let heartbeat_mavlink_information = mavlink_camera_information.clone();
        let receive_message_mavlink_information = mavlink_camera_information.clone();

        Self {
            mavlink_camera_information: mavlink_camera_information.clone(),
            heartbeat_state: heartbeat_state.clone(),
            heartbeat_thread: std::thread::spawn(move || {
                heartbeat_loop(heartbeat_state.clone(), heartbeat_mavlink_information)
            }),
            receive_message_state: receive_message_state.clone(),
            receive_message_thread: std::thread::spawn(move || {
                receive_message_loop(
                    receive_message_state.clone(),
                    receive_message_mavlink_information,
                )
            }),
        }
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

        let mut heartbeat_state = atomic_thread_state.as_ref().lock().unwrap();
        if *heartbeat_state == ThreadState::ZOMBIE {
            continue;
        }
        if *heartbeat_state == ThreadState::DEAD {
            break;
        }

        if *heartbeat_state == ThreadState::RESTART {
            *heartbeat_state = ThreadState::RUNNING;
            drop(heartbeat_state);

            std::thread::sleep(std::time::Duration::from_secs(3));
            continue;
        }

        println!("send!");
        if let Err(error) = vehicle
            .as_ref()
            .lock()
            .unwrap()
            .send(&header, &heartbeat_message())
        {
            eprintln!("Failed to send heartbeat: {:?}", error);
        }
    }
}

fn receive_message_loop(
    atomic_thread_state: Arc<Mutex<ThreadState>>,
    mavlink_camera_information: Arc<Mutex<MavlinkCameraInformation>>,
) {
}

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
