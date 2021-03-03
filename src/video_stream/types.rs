use crate::stream::types::StreamInformation;
use crate::video::types::VideoSourceType;

use serde::{Deserialize, Serialize};

//TODO: move to stream ?
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VideoAndStreamInformation {
    pub name: String,
    pub stream_information: StreamInformation,
    pub video_source: VideoSourceType,
}
