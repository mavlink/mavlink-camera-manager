use toml;

use serde::{Deserialize, Serialize};

use std::io::prelude::*;
use std::sync::{Arc, Once};

#[derive(Debug, Deserialize, Serialize)]
struct HeaderSettingsFile {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct VideoConfiguration {
    pub device: String,
    pub pipeline: String,
    pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SettingsStruct {
    pub header: HeaderSettingsFile,
    pub mavlink_endpoint: String,
    pub videos_configuration: Vec<VideoConfiguration>,
}

impl Default for SettingsStruct {
    fn default() -> Self {
        SettingsStruct {
            header: HeaderSettingsFile {
                name: "Camera Manager".to_string(),
            },
            mavlink_endpoint: "udpout:0.0.0.0:14550".to_string(),
            videos_configuration: vec![],
        }
    }
}

#[derive(Debug)]
struct Settings {
    pub file_name: String,
    config_arc: Arc<SettingsStruct>,
}

impl Settings {
    pub fn new(file_name: &str) -> Self {
        let content = std::fs::read_to_string(file_name).unwrap_or_else(|_| {
            Settings::create_settings_file(file_name);
            std::fs::read_to_string(file_name).unwrap()
        });
        let content: SettingsStruct = toml::from_str(&content.as_str()).unwrap_or_else( |_| {
            Settings::create_settings_file(file_name);
            let content = std::fs::read_to_string(file_name).unwrap();
            toml::from_str(&content.as_str()).unwrap()
        });

        println!("> {:#?}", &content);

        let mut settings = Settings {
            file_name: file_name.to_string(),
            config_arc: Arc::new(content),
        };

        /*
        Arc::make_mut(&mut settings.config_arc)
            .merge(config::File::with_name(&settings.file_name.as_str()))
            .unwrap_or_else(|error| panic!("Failed to load settings file: {}", error));
        */

        return settings;
    }

    pub fn create_settings_file(file_name: &str) {
        let mut file = std::fs::File::create(file_name).unwrap();
        let value = toml::Value::try_from(SettingsStruct::default()).unwrap();
        file.write_all(value.to_string().as_bytes()).unwrap();
    }
}

#[test]
fn constructor() {
    // Test file that does not exist
    let settings = Settings::new("/tmp/test.toml");
    // Test file that exist
    let settings = Settings::new("/tmp/test.toml");
}
