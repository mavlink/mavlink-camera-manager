use crate::stream::types::StreamInformation;
use crate::video::types::VideoSourceType;

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

use std::collections::HashSet;

use simple_error::SimpleError;
//TODO: move to stream ?
#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VideoAndStreamInformation {
    pub name: String,
    pub stream_information: StreamInformation,
    pub video_source: VideoSourceType,
}

impl VideoAndStreamInformation {
    pub fn conflicts_with(&self, other: &VideoAndStreamInformation) -> Result<(), SimpleError> {
        if self.name == other.name {
            return Err(SimpleError::new(format!(
                "Streams have same name: {}",
                self.name
            )));
        }

        if (!self.video_source.inner().is_shareable())
            && (self.video_source.inner().source_string()
                == other.video_source.inner().source_string())
        {
            return Err(SimpleError::new(format!(
                "Streams have same source: {}",
                self.video_source.inner().source_string()
            )));
        }

        let our_endpoints: HashSet<_> = self.stream_information.endpoints.iter().collect();
        let other_endpoints: HashSet<_> = other.stream_information.endpoints.iter().collect();
        let common_endpoints: HashSet<_> = our_endpoints.intersection(&other_endpoints).collect();

        if !common_endpoints.is_empty() {
            return Err(SimpleError::new(format!(
                "Stream ({other_name} - {other_source}) has common endpoint with Stream ({our_name} - {our_source}) to already existing stream: {endpoints:#?}",
                other_name = other.name,
                other_source = other.video_source.inner().source_string(),
                our_name = self.name,
                our_source = self.video_source.inner().source_string(),
                endpoints = common_endpoints
            )));
        }

        return Ok(());
    }
}

//TODO: Add tests
