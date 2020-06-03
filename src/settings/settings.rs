use toml;

use serde::{Deserialize, Serialize};

use std::io::prelude::*;

use notify;
use std::sync::mpsc;

use derivative::Derivative;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HeaderSettingsFile {
    pub name: String,
    pub version: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VideoConfiguration {
    pub device: String,
    pub pipeline: Option<String>,
    pub endpoint: Option<String>, //TODO: Move to struct
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SettingsStruct {
    pub header: HeaderSettingsFile,
    pub mavlink_endpoint: String,
    pub videos_configuration: Vec<VideoConfiguration>,
}

impl Default for SettingsStruct {
    fn default() -> Self {
        SettingsStruct {
            header: HeaderSettingsFile {
                name: "Camera Manager".to_string(),
                version: 0,
            },
            mavlink_endpoint: "udpout:0.0.0.0:14550".to_string(),
            videos_configuration: vec![],
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct Settings {
    pub file_name: String,
    pub config: SettingsStruct,
    pub file_channel: mpsc::Receiver<notify::DebouncedEvent>,

    #[derivative(Debug = "ignore")]
    watcher: notify::RecommendedWatcher,
}

impl Settings {
    pub fn new(file_name: &str) -> Self {
        let settings = Settings::load_settings_from_file(file_name);
        let (tx, rx) = mpsc::channel();

        let settings = Settings {
            file_name: file_name.to_string(),
            config: settings,
            file_channel: rx,
            watcher: notify::Watcher::new(tx, std::time::Duration::from_secs(1)).unwrap(),
        };

        settings.save().unwrap_or_else(|error| {
            println!("Failed to save file: {:#?}", error);
        });

        return settings;
    }

    pub fn load_settings_from_file(file_name: &str) -> SettingsStruct {
        let result = std::fs::read_to_string(file_name);

        if (result.is_err()) {
            return SettingsStruct::default();
        };

        return toml::from_str(&result.unwrap().as_str())
            .unwrap_or_else(|x| SettingsStruct::default());
    }

    pub fn load(&mut self) {
        let settings = Settings::load_settings_from_file(&self.file_name);
        self.config = settings;
    }

    pub fn save_settings_to_file(file_name: &str, content: &SettingsStruct) -> std::io::Result<()> {
        let mut file = std::fs::File::create(file_name).unwrap();
        let value = toml::Value::try_from(content).unwrap();
        file.write_all(value.to_string().as_bytes())
    }

    pub fn save(&self) -> std::io::Result<()> {
        Settings::save_settings_to_file(&self.file_name, &self.config)
    }
}

#[cfg(test)]
use rand::Rng;

#[test]
fn simple_test() {
    let rand_string: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(30)
        .collect();

    let file_name = format!("/tmp/{}.toml", rand_string);
    println!("Test file: {}", file_name);

    let mut settings = Settings::new(&file_name);
    settings.config.header.name = "test".to_string();
    settings.save().unwrap();

    let settings = Settings::new(&file_name);
    assert_eq!(settings.config.header.name, "test".to_string());
}
