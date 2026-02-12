use anyhow::Result;

use mcm_api::v1::{controls::Control, stream::VideoCaptureConfiguration, video::*};

use crate::video::{
    types::VideoSourceTypeExt,
    video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats},
    video_source_local::{VideoSourceLocal, VideoSourceLocalExt, VideoSourceLocalType},
};

impl VideoSourceLocalExt for VideoSourceLocal {
    async fn try_identify_device(
        &mut self,
        _capture_configuration: &VideoCaptureConfiguration,
        _candidates: &[VideoSourceType],
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

impl VideoSourceFormats for VideoSourceLocal {
    async fn formats(&self) -> Vec<Format> {
        vec![]
    }
}

impl VideoSource for VideoSourceLocal {
    fn name(&self) -> &String {
        &self.name
    }

    fn source_string(&self) -> &str {
        &self.device_path
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
    async fn cameras_available() -> Vec<VideoSourceType> {
        return vec![];
    }
}
