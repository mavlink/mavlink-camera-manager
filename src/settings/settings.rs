use toml;

use serde::{Deserialize, Serialize};

use std::io::prelude::*;
use std::sync::{Arc, Once};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HeaderSettingsFile {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct VideoConfiguration {
    pub device: String,
    pub pipeline: String,
    pub endpoint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
    pub config_arc: Arc<SettingsStruct>,
}

impl Settings {
    pub fn new(file_name: &str) -> Self {
        let settings = Settings::load_settings_from_file(file_name);

        let mut settings = Settings {
            file_name: file_name.to_string(),
            config_arc: Arc::new(settings),
        };

        return settings;
    }

    pub fn load_settings_from_file(file_name: &str) -> SettingsStruct {
        let result = std::fs::read_to_string(file_name);

        if (result.is_err()) {
            return SettingsStruct::default();
        };

        return toml::from_str(&result.unwrap().as_str()).unwrap_or_else(|x|{
            SettingsStruct::default()
        });
    }

    pub fn save_settings_to_file(file_name: &str, content: &SettingsStruct) -> std::io::Result<()> {
        let mut file = std::fs::File::create(file_name).unwrap();
        let value = toml::Value::try_from(content).unwrap();
        file.write_all(value.to_string().as_bytes())
    }

    pub fn create_settings_file(file_name: &str) -> std::io::Result<()> {
        Settings::save_settings_to_file(file_name, &SettingsStruct::default())
    }

    pub fn save(&self) -> std::io::Result<()> {
        Settings::save_settings_to_file(&self.file_name, Arc::as_ref(&self.config_arc))
    }
}

#[test]
fn simple_test() {
    let mut settings = Settings::new("/tmp/test.toml");
    Arc::make_mut(&mut settings.config_arc).header.name = "test".to_string();
    settings.save();

    let mut settings = Settings::new("/tmp/test.toml");
    assert!(Arc::as_ref(&settings.config_arc).header.name == "test".to_string());
}
