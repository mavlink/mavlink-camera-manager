use anyhow::{anyhow, Error, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::io::prelude::*;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tracing::*;

use crate::cli;
use crate::custom;
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
    static ref MANAGER: Arc<RwLock<Manager>> = Arc::new(RwLock::new(Manager { content: None }));
}

impl Default for SettingsStruct {
    fn default() -> Self {
        SettingsStruct {
            header: HeaderSettingsFile {
                name: "Camera Manager".to_string(),
                version: 0,
            },
            mavlink_endpoint: cli::manager::mavlink_connection_string(),
            streams: custom::create_default_streams(),
        }
    }
}

impl Manager {
    fn with(file_name: &str) -> ManagerStruct {
        let file_name = if !Path::new(file_name).is_absolute() {
            match ProjectDirs::from("com", "Blue Robotics", env!("CARGO_PKG_NAME")) {
                Some(project) => {
                    let folder_path = Path::new(project.config_dir());
                    if let Err(error) = std::fs::create_dir_all(folder_path) {
                        error!(
                            "Failed to create settings folder: {folder_path:?}. Reason: {error:#?}"
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

        let config = if cli::manager::is_reset() {
            debug!("Settings reset, an empty settings will be loaded and stored as {file_name:?}.");
            fallback_settings_with_backup_file(&file_name)
        } else {
            debug!("Using settings file: {file_name:?}");
            load_settings_from_file(&file_name)
        };

        let settings = ManagerStruct {
            file_name: file_name.to_string(),
            config,
        };

        create_directories(&settings.file_name).unwrap_or_else(|error| {
            error!("Failed to create parent directories for {file_name:?}. Reason: {error:#?}");
        });

        save_settings_to_file(&settings.file_name, &settings.config).unwrap_or_else(|error| {
            error!("Failed to save file {file_name:?}. Reason: {error:#?}");
        });

        settings
    }
}

// Init settings manager with the desired settings file,
// will be created if does not exist
pub fn init(file_name: Option<&str>) {
    let mut manager = MANAGER.write().unwrap();
    let file_name = file_name.unwrap_or("settings.json");
    manager.content = Some(Manager::with(file_name));
}

fn fallback_settings_with_backup_file(file_name: &str) -> SettingsStruct {
    let backup_file_name = format!("{file_name}.bak");

    if std::fs::metadata(file_name).is_ok() {
        info!("The settings file {file_name:?} will be backed-up as {backup_file_name:?}, and a new (empty) settings file will be created in its place.");

        if let Err(error) = std::fs::copy(file_name, backup_file_name.as_str()) {
            error!("Failed to create backup file {backup_file_name:?}. Reason: {error:#?}");
        }
    }

    SettingsStruct::default()
}

fn load_settings_from_file(file_name: &str) -> SettingsStruct {
    std::fs::read_to_string(file_name)
        .map_err(Error::msg)
        .and_then(|value| serde_json::from_str(&value).map_err(Error::msg))
        .unwrap_or_else(|error| {
            warn!("Failed to load settings file {file_name:?}. Reason: {error}");
            fallback_settings_with_backup_file(file_name)
        })
}

fn create_directories(file_name: &str) -> Result<()> {
    let path = Path::new(&file_name);
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return Err(anyhow!("Error creating directories: {error:#?}"));
        }
    }

    Ok(())
}

fn save_settings_to_file(file_name: &str, content: &SettingsStruct) -> Result<()> {
    let json = serde_json::to_string_pretty(content)?;

    let mut file = std::fs::File::create(file_name)?;

    file.write_all(json.as_bytes())?;

    Ok(())
}

// Save the latest state of the settings
pub fn save() {
    let manager = MANAGER.read().unwrap();
    //TODO: deal com save problems here
    if let Some(content) = &manager.content {
        if let Err(error) = save_settings_to_file(&content.file_name, &content.config) {
            error!(
                "Failed to save settings: file: {:#?}, configuration: {:#?}, error: {:#?}",
                &content.file_name, &content.config, error
            );
            return;
        }
        debug!("Settings saved: {content:#?}");
    } else {
        warn!("Skipped saving: the configuration (manager.content) was None");
    }
}

#[allow(dead_code)]
pub fn header() -> HeaderSettingsFile {
    let manager = MANAGER.read().unwrap();
    manager.content.as_ref().unwrap().config.header.clone()
}

pub fn mavlink_endpoint() -> String {
    let manager = MANAGER.read().unwrap();
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
        let mut manager = MANAGER.write().unwrap();
        let mut content = manager.content.as_mut();
        content.as_mut().unwrap().config.mavlink_endpoint = endpoint.into();
    }
    save();
}

pub fn streams() -> Vec<VideoAndStreamInformation> {
    let manager = MANAGER.read().unwrap();
    let content = manager.content.as_ref();
    content.unwrap().config.streams.clone()
}

pub fn set_streams(streams: &[VideoAndStreamInformation]) {
    // Take care of scope RwLock
    {
        let mut manager = MANAGER.write().unwrap();
        let mut content = manager.content.as_mut();
        content.as_mut().unwrap().config.streams.clear();
        content
            .as_mut()
            .unwrap()
            .config
            .streams
            .append(&mut streams.to_owned());
    }
    save();
}

pub fn reset() {
    // Take care of scope RwLock
    {
        let mut manager = MANAGER.write().unwrap();
        manager.content.as_mut().unwrap().config = SettingsStruct::default();
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

        format!("/tmp/{}.json", rand_string)
    }

    #[test]
    fn test_no_aboslute_path() {
        init(None);
        let manager = MANAGER.read().unwrap();
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
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
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
        set_streams(&fake_streams);
        assert_eq!(streams(), fake_streams);

        save();
    }
}
