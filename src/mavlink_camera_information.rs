use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct MavlinkCameraInformation {
    system_id: u8,
    component_id: u8,
    // This is necessary since mavlink does not provide a way to have nonblock communication
    // So we need the main mavlink loop live while changing the stream uri
    // and forcing QGC to request the new uri available
    video_stream_uri: Arc<Mutex<String>>,
    verbose: bool,
    vehicle:
        Option<Arc<Box<dyn mavlink::MavConnection<mavlink::common::MavMessage> + Sync + Send>>>,
    // Counter used to not emit heartbeats to force QGC update
    restart_counter: Arc<Mutex<u8>>,
}

impl Default for MavlinkCameraInformation {
    fn default() -> Self {
        MavlinkCameraInformation {
            system_id: 1,
            component_id: mavlink::common::MavComponent::MAV_COMP_ID_CAMERA as u8,
            video_stream_uri: Arc::new(Mutex::new(
                "rtsp://wowzaec2demo.streamlock.net:554/vod/mp4:BigBuckBunny_115k.mov".to_string(),
            )),
            verbose: false,
            vehicle: Default::default(),
            restart_counter: Arc::new(Mutex::new(0)),
        }
    }
}

impl MavlinkCameraInformation {
    pub fn connect(&mut self, connection_string: &str) {
        let mut mavlink_connection = mavlink::connect(&connection_string).unwrap();
        mavlink_connection.set_protocol_version(mavlink::MavlinkVersion::V2);
        self.vehicle = Some(Arc::new(mavlink_connection));
    }

    pub fn set_video_stream_uri(&self, uri: String) {
        *self.video_stream_uri.lock().unwrap() = uri;
        *self.restart_counter.lock().unwrap() = 5;
    }

    pub fn set_verbosity(&mut self, verbose: bool) {
        self.verbose = verbose;
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

    fn camera_information(&self) -> mavlink::common::MavMessage {
        // Create a fixed size array with the camera name
        let name_str = String::from("name");
        let mut name: [u8; 32] = [0; 32];
        for index in 0..name_str.len() as usize {
            name[index] = name_str.as_bytes()[index];
        }

        // Send path to our camera configuration file
        let uri: Vec<char> = format!("{}", "http://0.0.0.0").chars().collect();

        // Send fake data
        mavlink::common::MavMessage::CAMERA_INFORMATION(mavlink::common::CAMERA_INFORMATION_DATA {
            time_boot_ms: 0,
            firmware_version: 0,
            focal_length: 0.0,
            sensor_size_h: 0.0,
            sensor_size_v: 0.0,
            flags: mavlink::common::CameraCapFlags::CAMERA_CAP_FLAGS_HAS_VIDEO_STREAM,
            resolution_h: 0,
            resolution_v: 0,
            cam_definition_version: 0,
            vendor_name: name,
            model_name: name,
            lens_id: 0,
            cam_definition_uri: uri,
        })
    }

    fn camera_settings(&self) -> mavlink::common::MavMessage {
        //Send fake data
        mavlink::common::MavMessage::CAMERA_SETTINGS(mavlink::common::CAMERA_SETTINGS_DATA {
            time_boot_ms: 0,
            zoomLevel: 0.0,
            focusLevel: 0.0,
            mode_id: mavlink::common::CameraMode::CAMERA_MODE_VIDEO,
        })
    }

    fn camera_storage_information(&self) -> mavlink::common::MavMessage {
        //Send fake data
        mavlink::common::MavMessage::STORAGE_INFORMATION(
            mavlink::common::STORAGE_INFORMATION_DATA {
                time_boot_ms: 0,
                total_capacity: 102400.0,
                used_capacity: 0.0,
                available_capacity: 102400.0,
                read_speed: 1000.0,
                write_speed: 1000.0,
                storage_id: 0,
                storage_count: 0,
                status: mavlink::common::StorageStatus::STORAGE_STATUS_READY,
            },
        )
    }

    fn camera_capture_status(&self) -> mavlink::common::MavMessage {
        //Send fake data
        mavlink::common::MavMessage::CAMERA_CAPTURE_STATUS(
            mavlink::common::CAMERA_CAPTURE_STATUS_DATA {
                time_boot_ms: 0,
                image_interval: 0.0,
                recording_time_ms: 0,
                available_capacity: 10000.0,
                image_status: 0,
                video_status: 0,
            },
        )
    }

    fn video_stream_information(&self) -> mavlink::common::MavMessage {
        let name_str = String::from("name");
        let mut name: [char; 32] = ['\0'; 32];
        for index in 0..name_str.len() as u32 {
            name[index as usize] = name_str.as_bytes()[index as usize] as char;
        }

        let uri: Vec<char> = format!("{}\0", self.video_stream_uri.lock().unwrap())
            .chars()
            .collect();

        //The only important information here is the mavtype and uri variables, everything else is fake
        mavlink::common::MavMessage::VIDEO_STREAM_INFORMATION(
            mavlink::common::VIDEO_STREAM_INFORMATION_DATA {
                framerate: 30.0,
                bitrate: 1000,
                flags: mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_RUNNING,
                resolution_h: 1000,
                resolution_v: 1000,
                rotation: 0,
                hfov: 0,
                stream_id: 1, // Starts at 1, 0 is for broadcast
                count: 0,
                mavtype: mavlink::common::VideoStreamType::VIDEO_STREAM_TYPE_RTSP,
                name: name,
                uri: uri,
            },
        )
    }

    pub fn run_loop(&self) {
        let mut header = mavlink::MavHeader::default();
        header.system_id = self.system_id;
        header.component_id = self.component_id;

        // Create heartbeat thread
        thread::spawn({
            let vehicle = self.vehicle.as_ref().unwrap().clone();
            let restart_counter = self.restart_counter.clone();
            move || loop {
                thread::sleep(Duration::from_secs(1));
                let mut restart_counter = restart_counter.lock().unwrap();
                if *restart_counter > 0 {
                    println!("Restarting camera service in {} seconds.", restart_counter);
                    *restart_counter -= 1;
                    continue;
                }

                let res = vehicle.send(&header, &MavlinkCameraInformation::heartbeat_message());
                if res.is_err() {
                    println!("Failed to send heartbeat: {:?}", res);
                }
            }
        });

        // Our main loop
        loop {
            match self.vehicle.as_ref().unwrap().recv() {
                Ok((_header, msg)) => {
                    let vehicle = self.vehicle.as_ref().unwrap();
                    match msg {
                        // Check if there is any camera information request from gcs
                        mavlink::common::MavMessage::COMMAND_LONG(command_long) => {
                            match command_long.command {
                                mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_INFORMATION => {
                                    println!("Sending camera_information..");
                                    let res = vehicle.send(&header, &self.camera_information());
                                    if res.is_err() {
                                        println!("Failed to send camera_information: {:?}", res);
                                    }
                                },
                                mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_SETTINGS => {
                                    println!("Sending camera_settings..");
                                    let res = vehicle.send(&header, &self.camera_settings());
                                    if res.is_err() {
                                        println!("Failed to send camera_settings: {:?}", res);
                                    }
                                }
                                mavlink::common::MavCmd::MAV_CMD_REQUEST_STORAGE_INFORMATION => {
                                    println!("Sending camera_storage_information..");
                                    let res = vehicle.send(&header, &self.camera_storage_information());
                                    if res.is_err() {
                                        println!("Failed to send camera_storage_information: {:?}", res);
                                    }
                                }
                                mavlink::common::MavCmd::MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS => {
                                    println!("Sending camera_capture_status..");
                                    let res = vehicle.send(&header, &self.camera_capture_status());
                                    if res.is_err() {
                                        println!("Failed to send camera_capture_status: {:?}", res);
                                    }
                                }
                                mavlink::common::MavCmd::MAV_CMD_REQUEST_VIDEO_STREAM_INFORMATION => {
                                    println!("Sending video_stream_information..");
                                    let res = vehicle.send(&header, &self.video_stream_information());
                                    if res.is_err() {
                                        println!("Failed to send video_stream_information: {:?}", res);
                                    }
                                }
                                _ => {
                                    if self.verbose {
                                        println!("Ignoring command: {:?}", command_long.command);
                                    }
                                }
                            }
                        }
                        // We receive a bunch of heartbeat messages, we can ignore it
                        mavlink::common::MavMessage::HEARTBEAT(_) => {}
                        // Any other message that is not a heartbeat or command_long
                        _ => {
                            if self.verbose {
                                println!("Ignoring: {:?}", msg);
                            }
                        }
                    }
                }
                Err(e) => {
                    println!("recv error: {:?}", e);
                }
            }
        }
    }
}
