use crate::stream::types::StreamInformation;
use crate::video::types::VideoSourceType;

//TODO: move to stream ?
pub struct VideoAndStreamInformation {
    pub name: String,
    pub stream_information: StreamInformation,
    pub video_source: VideoSourceType,
}
