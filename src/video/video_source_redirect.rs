use super::types::*;
use super::video_source::{VideoSource, VideoSourceAvailable};

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use tracing::*;

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceRedirectType {
    Redirect(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSourceRedirect {
    pub name: String,
    pub source: VideoSourceRedirectType,
}

impl VideoSource for VideoSourceRedirect {
    #[instrument(level = "debug")]
    fn name(&self) -> &String {
        &self.name
    }

    #[instrument(level = "debug")]
    fn source_string(&self) -> &str {
        match &self.source {
            VideoSourceRedirectType::Redirect(string) => string,
        }
    }

    #[instrument(level = "debug")]
    fn formats(&self) -> Vec<Format> {
        match &self.source {
            VideoSourceRedirectType::Redirect(_) => {
                vec![]
            }
        }
    }

    #[instrument(level = "debug")]
    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    #[instrument(level = "debug")]
    fn set_control_by_id(&self, _control_id: u64, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    #[instrument(level = "debug")]
    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    #[instrument(level = "debug")]
    fn control_value_by_id(&self, _control_id: u64) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    #[instrument(level = "debug")]
    fn controls(&self) -> Vec<Control> {
        vec![]
    }

    #[instrument(level = "debug")]
    fn is_valid(&self) -> bool {
        match &self.source {
            VideoSourceRedirectType::Redirect(_) => true,
        }
    }

    #[instrument(level = "debug")]
    fn is_shareable(&self) -> bool {
        true
    }
}

impl VideoSourceAvailable for VideoSourceRedirect {
    #[instrument(level = "debug")]
    fn cameras_available() -> Vec<VideoSourceType> {
        vec![VideoSourceType::Redirect(VideoSourceRedirect {
            name: "Redirect source".into(),
            source: VideoSourceRedirectType::Redirect("Redirect".into()),
        })]
    }
}
