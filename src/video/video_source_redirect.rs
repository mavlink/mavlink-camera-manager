use super::types::*;
use super::video_source::{VideoSource, VideoSourceAvailable};

use anyhow::Result;
use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use tracing::*;

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceRedirectType {
    Redirect(String),
    Onvif(url::Url),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSourceRedirect {
    pub name: String,
    pub source: VideoSourceRedirectType,
}

impl VideoSource for VideoSourceRedirect {
    fn name(&self) -> &String {
        &self.name
    }

    fn source_string(&self) -> &str {
        match &self.source {
            VideoSourceRedirectType::Redirect(string) => string,
            VideoSourceRedirectType::Onvif(url) => url.as_str(),
        }
    }

    fn formats(&self) -> Vec<Format> {
        match &self.source {
            VideoSourceRedirectType::Redirect(_) | VideoSourceRedirectType::Onvif(_) => {
                vec![]
            }
        }
    }

    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    fn set_control_by_id(&self, _control_id: u64, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    fn control_value_by_id(&self, _control_id: u64) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Redirect source doesn't have controls.",
        ))
    }

    fn controls(&self) -> Vec<Control> {
        vec![]
    }

    fn is_valid(&self) -> bool {
        match &self.source {
            VideoSourceRedirectType::Redirect(_) | VideoSourceRedirectType::Onvif(_) => true,
        }
    }

    fn is_shareable(&self) -> bool {
        true
    }
}

impl VideoSourceAvailable for VideoSourceRedirect {
    fn cameras_available() -> Vec<VideoSourceType> {
        let mut sources = vec![VideoSourceType::Redirect(VideoSourceRedirect {
            name: "Redirect source".into(),
            source: VideoSourceRedirectType::Redirect("Redirect".into()),
        })];

        // TODO: get onvif cameras from static cache

        sources
    }
}
