use mcm_api::v1::{controls::Control, video::*};

use super::video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats};

impl VideoSourceFormats for VideoSourceRedirect {
    async fn formats(&self) -> Vec<Format> {
        match &self.source {
            VideoSourceRedirectType::Redirect(_) => {
                vec![]
            }
        }
    }
}

impl VideoSource for VideoSourceRedirect {
    fn name(&self) -> &String {
        &self.name
    }

    fn source_string(&self) -> &str {
        match &self.source {
            VideoSourceRedirectType::Redirect(string) => string,
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
            VideoSourceRedirectType::Redirect(_) => true,
        }
    }

    fn is_shareable(&self) -> bool {
        true
    }
}

impl VideoSourceAvailable for VideoSourceRedirect {
    async fn cameras_available() -> Vec<VideoSourceType> {
        vec![VideoSourceType::Redirect(VideoSourceRedirect {
            name: "Redirect source".into(),
            source: VideoSourceRedirectType::Redirect("Redirect".into()),
        })]
    }
}
