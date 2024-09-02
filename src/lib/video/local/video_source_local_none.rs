use crate::controls::types::Control;
use crate::stream::types::VideoCaptureConfiguration;
use crate::video::types::*;
use crate::video::video_source::{VideoSource, VideoSourceAvailable};

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

use anyhow::Result;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceLocalType {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSourceLocal {
    pub name: String,
    pub device_path: String,
}

impl VideoSourceLocal {
    pub fn try_identify_device(
        &mut self,
        capture_configuration: &VideoCaptureConfiguration,
        candidates: &[VideoSourceType],
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

impl VideoSource for VideoSourceLocal {
    fn name(&self) -> &String {
        &self.name
    }

    fn source_string(&self) -> &str {
        &self.device_path
    }

    fn formats(&self) -> Vec<Format> {
        return vec![];
    }

    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "None source doesn't have controls.",
        ))
    }

    fn set_control_by_id(&self, _control_id: u64, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "None source doesn't have controls.",
        ))
    }

    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "None source doesn't have controls.",
        ))
    }

    fn control_value_by_id(&self, _control_id: u64) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "None source doesn't have controls.",
        ))
    }

    fn controls(&self) -> Vec<Control> {
        vec![]
    }

    fn is_valid(&self) -> bool {
        false
    }

    fn is_shareable(&self) -> bool {
        true
    }
}

impl VideoSourceAvailable for VideoSourceLocal {
    fn cameras_available() -> Vec<VideoSourceType> {
        return vec![];
    }
}
