use toml;

use log::*;
use serde::{Deserialize, Serialize};
use std::io::prelude::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HeaderSettingsFile {
    pub name: String,
    pub version: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VideoConfiguration {
    pub device: Option<String>,
    pub pipeline: Option<String>,
    pub endpoints: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SettingsStruct {
    pub header: HeaderSettingsFile,
    pub mavlink_endpoint: String,
    pub videos_configuration: Vec<VideoConfiguration>,
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
            videos_configuration: vec![
                VideoConfiguration {
                    device: None,
                    pipeline: Some("videotestsrc pattern=snow ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! rtph264pay ! udpsink host=0.0.0.0 port=5600".to_string()),
                    endpoints: vec!["udp://0.0.0.0:5600".to_string()],
                }
            ],
        }
    }
}

impl Manager {
    fn new(file_name: &str) -> ManagerStruct {
        use directories::ProjectDirs;
        use std::path::Path;

        let file_name = if !Path::new(file_name).is_absolute() {
            match ProjectDirs::from("com", "Blue Robotics", env!("CARGO_PKG_NAME")) {
                Some(project) => Path::new(project.config_dir())
                    .join(file_name)
                    .to_str()
                    .expect("Failed to create settings path.")
                    .to_string(),
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
    let file_name = file_name.unwrap_or("settings.toml");
    manager.content = Some(Manager::new(file_name));
}

fn load_settings_from_file(file_name: &str) -> SettingsStruct {
    let result = std::fs::read_to_string(file_name);

    debug!("loaded!");
    if result.is_err() {
        return SettingsStruct::default();
    };

    return toml::from_str(&result.unwrap().as_str())
        .unwrap_or_else(|_error| SettingsStruct::default());
}

//TODO: remove allow dead code
#[allow(dead_code)]
fn load() {
    let mut manager = MANAGER.as_ref().lock().unwrap();
    //TODO: deal with load problems
    if let Some(content) = &mut manager.content {
        content.config = load_settings_from_file(&content.file_name);
    }
}

fn save_settings_to_file(file_name: &str, content: &SettingsStruct) -> std::io::Result<()> {
    let mut file = std::fs::File::create(file_name)?;
    let value = toml::Value::try_from(content).unwrap();
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

#[test]
fn simple_test() {
    use rand::Rng;

    let rand_string: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(30)
        .map(char::from)
        .collect();

    let file_name = format!("/tmp/{}.toml", rand_string);
    println!("Test file: {}", &file_name);
    init(Some(&file_name));
    save();

    let settings = Manager::new(&file_name);
    assert_eq!(settings.config.header.name, "Camera Manager".to_string());

    //TODO Add write/read test
}
