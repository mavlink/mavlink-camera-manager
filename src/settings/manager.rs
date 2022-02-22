use directories::ProjectDirs;
use log::*;
use serde::{Deserialize, Serialize};
use std::io::prelude::*;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::cli;
use crate::video_stream::types::VideoAndStreamInformation;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HeaderSettingsFile {
    pub name: String,
    pub version: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SettingsStruct {
    pub header: HeaderSettingsFile,
    pub mavlink_endpoint: String, //TODO: Move to URL
    pub streams: Vec<VideoAndStreamInformation>,
}

#[derive(Debug)]
struct ManagerStruct {
    pub file_name: String,
    pub config: SettingsStruct,
}

struct Manager {
    pub content: Option<ManagerStruct>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager { content: None }));
}

impl Default for SettingsStruct {
    fn default() -> Self {
        SettingsStruct {
            header: HeaderSettingsFile {
                name: "Camera Manager".to_string(),
                version: 0,
            },
            mavlink_endpoint: "udpout:0.0.0.0:14550".to_string(),
            streams: vec![/*VideoAndStreamInformation {
                name: "Test".into(),
                stream_information: StreamInformation {
                    endpoints: vec![Url::parse("udp://0.0.0.0:5601").unwrap()],
                    configuration: CaptureConfiguration {
                        encode: VideoEncodeType::H264,
                        height: 720,
                        width: 1080,
                        frame_interval: FrameInterval {
                            numerator: 1,
                            denominator: 30,
                        },
                    },
                },
                video_source: VideoSourceType::Local(VideoSourceLocal {
                    name: "Camera Manager Default Camera".into(),
                    device_path: "/dev/video0".into(),
                    typ: VideoSourceLocalType::Usb("0000:08:00.3-1".into()),
                }),
            }*/],
        }
    }
}

impl Manager {
    fn new(file_name: &str) -> ManagerStruct {
        let file_name = if !Path::new(file_name).is_absolute() {
            match ProjectDirs::from("com", "Blue Robotics", env!("CARGO_PKG_NAME")) {
                Some(project) => {
                    let folder_path = Path::new(project.config_dir());
                    if let Err(error) = std::fs::create_dir_all(folder_path) {
                        error!(
                            "Failed to create settings folder: {}, reason: {:#?}",
                            folder_path.to_str().unwrap(),
                            error
                        );
                    }
                    Path::new(&folder_path)
                        .join(file_name)
                        .to_str()
                        .expect("Failed to create settings path.")
                        .to_string()
                }
                None => panic!("Failed to find user settings path."),
            }
        } else {
            file_name.into()
        };

        debug!("Using settings file: {}", &file_name);

        let settings = load_settings_from_file(&file_name);

        let settings = ManagerStruct {
            file_name: file_name.to_string(),
            config: settings,
        };

        save_settings_to_file(&settings.file_name, &settings.config).unwrap_or_else(|error| {
            error!("Failed to save file: {:#?}", error);
        });

        return settings;
    }
}

// Init settings manager with the desired settings file,
// will be created if does not exist
pub fn init(file_name: Option<&str>) {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    let file_name = file_name.unwrap_or("settings.json");
    manager.content = Some(Manager::new(file_name));
}

fn load_settings_from_file(file_name: &str) -> SettingsStruct {
    let result = std::fs::read_to_string(file_name);

    debug!("loaded!");
    if result.is_err() || cli::manager::is_reset() {
        return SettingsStruct::default();
    };

    return serde_json::from_str(&result.unwrap().as_str())
        .unwrap_or_else(|_error| SettingsStruct::default());
}

//TODO: remove allow dead code
#[allow(dead_code)]
fn load() {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    //TODO: deal with load problems
    if let Some(content) = &mut manager.content {
        content.config = load_settings_from_file(&content.file_name);
    } else {
        error!("Failed to load settings!");
    }
}

fn save_settings_to_file(file_name: &str, content: &SettingsStruct) -> std::io::Result<()> {
    let mut file = std::fs::File::create(file_name)?;
    debug!("content: {:#?}", content);
    let value = serde_json::to_string_pretty(content).unwrap();
    file.write_all(value.to_string().as_bytes())
}

// Save the latest state of the settings
pub fn save() {
    let manager = MANAGER.as_ref().lock().unwrap();
    //TODO: deal com save problems here
    if let Some(content) = &manager.content {
        if let Err(error) = save_settings_to_file(&content.file_name, &content.config) {
            error!(
                "Failed to save settings: file: {:#?}, configuration: {:#?}, error: {:#?}",
                &content.file_name, &content.config, error
            );
        }
    } else {
        debug!("saved!");
    }
}

#[allow(dead_code)]
pub fn header() -> HeaderSettingsFile {
    let manager = MANAGER.as_ref().lock().unwrap();
    return manager.content.as_ref().unwrap().config.header.clone();
}

pub fn mavlink_endpoint() -> String {
    let manager = MANAGER.as_ref().lock().unwrap();
    return manager
        .content
        .as_ref()
        .unwrap()
        .config
        .mavlink_endpoint
        .clone();
}

pub fn set_mavlink_endpoint(endpoint: &str) {
    //TODO: make content more easy to access
    {
        let mut manager = MANAGER.lock().unwrap();
        let mut content = manager.content.as_mut();
        content.as_mut().unwrap().config.mavlink_endpoint = endpoint.into();
    }
    save();
}

pub fn streams() -> Vec<VideoAndStreamInformation> {
    let manager = MANAGER.as_ref().lock().unwrap();
    let content = manager.content.as_ref();
    return content.unwrap().config.streams.clone();
}

pub fn set_streams(streams: &Vec<VideoAndStreamInformation>) {
    // Take care of scope mutex
    {
        let mut manager = MANAGER.lock().unwrap();
        let mut content = manager.content.as_mut();
        content.as_mut().unwrap().config.streams.clear();
        content
            .as_mut()
            .unwrap()
            .config
            .streams
            .append(&mut streams.clone());
    }
    save();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::types::{
        CaptureConfiguration, StreamInformation, VideoCaptureConfiguration,
    };
    use crate::video::{
        types::{FrameInterval, VideoEncodeType, VideoSourceType},
        video_source_local::{VideoSourceLocal, VideoSourceLocalType},
    };
    use url::Url;

    fn generate_random_settings_file_name() -> String {
        use rand::Rng;

        let rand_string: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(30)
            .map(char::from)
            .collect();

        return format!("/tmp/{}.json", rand_string);
    }

    #[test]
    fn test_no_aboslute_path() {
        init(None);
        let manager = MANAGER.as_ref().lock().unwrap();
        let file_name = &manager.content.as_ref().unwrap().file_name;
        assert!(
            std::path::Path::new(&file_name).exists(),
            "Settings file does not exist"
        );
    }

    #[test]
    fn test_store() {
        init(Some(&generate_random_settings_file_name()));

        let header = header();
        assert_eq!(header.name, "Camera Manager".to_string());

        let fake_mavlink_endpoint = "tcp:potatohost:42";
        set_mavlink_endpoint(fake_mavlink_endpoint);
        assert_eq!(mavlink_endpoint(), fake_mavlink_endpoint);

        let fake_streams = vec![VideoAndStreamInformation {
            name: "PotatoTestStream".into(),
            stream_information: StreamInformation {
                endpoints: vec![Url::parse("udp://potatohost:4242").unwrap()],
                configuration: CaptureConfiguration::VIDEO(VideoCaptureConfiguration {
                    encode: VideoEncodeType::H264,
                    height: 666,
                    width: 444,
                    frame_interval: FrameInterval {
                        numerator: 17,
                        denominator: 47,
                    },
                }),
                extended_configuration: None,
            },
            video_source: VideoSourceType::Local(VideoSourceLocal {
                name: "Fake Potato Test Video Source Camera".into(),
                device_path: "/dev/potatovideo".into(),
                typ: VideoSourceLocalType::Usb("usb-0420:08:47.42-77".into()),
            }),
        }];
        set_streams(&mut fake_streams.clone());
        assert_eq!(streams(), fake_streams);

        save();
    }
}
