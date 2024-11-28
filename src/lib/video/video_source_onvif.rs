use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

use crate::controls::{
    onvif::{camera::OnvifDeviceInformation, manager::Manager as OnvifManager},
    types::Control,
};

use super::{
    types::*,
    video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats},
};

#[derive(Apiv2Schema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceOnvifType {
    Onvif(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoSourceOnvif {
    pub name: String,
    pub source: VideoSourceOnvifType,
    #[serde(flatten)]
    pub device_information: OnvifDeviceInformation,
}

impl VideoSourceFormats for VideoSourceOnvif {
    async fn formats(&self) -> Vec<Format> {
        let VideoSourceOnvifType::Onvif(stream_uri) = &self.source;
        OnvifManager::get_formats(stream_uri)
            .await
            .unwrap_or_default()
    }
}

impl VideoSource for VideoSourceOnvif {
    fn name(&self) -> &String {
        &self.name
    }

    fn source_string(&self) -> &str {
        match &self.source {
            VideoSourceOnvifType::Onvif(url) => url.as_str(),
        }
    }

    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Onvif source doesn't have controls.",
        ))
    }

    fn set_control_by_id(&self, _control_id: u64, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Onvif source doesn't have controls.",
        ))
    }

    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Onvif source doesn't have controls.",
        ))
    }

    fn control_value_by_id(&self, _control_id: u64) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Onvif source doesn't have controls.",
        ))
    }

    fn controls(&self) -> Vec<Control> {
        vec![]
    }

    fn is_valid(&self) -> bool {
        match &self.source {
            VideoSourceOnvifType::Onvif(_) => true,
        }
    }

    fn is_shareable(&self) -> bool {
        true
    }
}

impl VideoSourceAvailable for VideoSourceOnvif {
    async fn cameras_available() -> Vec<VideoSourceType> {
        OnvifManager::streams_available().await.clone()
    }
}
