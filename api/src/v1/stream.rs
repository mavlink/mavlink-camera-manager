use serde::{Deserialize, Serialize};
use ts_rs::TS;
use url::Url;

use crate::v1::video::{FrameInterval, VideoEncodeType, VideoSourceType};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct VideoCaptureConfiguration {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
}

#[deprecated(note = "The API will soon allow for optional CaptureConfiguration instead")]
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct RedirectCaptureConfiguration {}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(tag = "type", rename_all = "lowercase")]
#[non_exhaustive]
pub enum CaptureConfiguration {
    Video(VideoCaptureConfiguration),
    /// This is only still used for easy stream creation, and it is always converted to Self::Video.
    #[allow(deprecated)]
    Redirect(RedirectCaptureConfiguration),
}

impl std::fmt::Display for CaptureConfiguration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Video(_) => write!(f, "Video"),
            Self::Redirect(_) => write!(f, "Redirect"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(default)]
#[non_exhaustive]
pub struct ExtendedConfiguration {
    pub thermal: bool,
    pub disable_mavlink: bool,
    pub disable_zenoh: bool,
    pub disable_thumbnails: bool,
}

impl ExtendedConfiguration {
    pub fn new(
        thermal: bool,
        disable_mavlink: bool,
        disable_zenoh: bool,
        disable_thumbnails: bool,
    ) -> Self {
        Self {
            thermal,
            disable_mavlink,
            disable_zenoh,
            disable_thumbnails,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub configuration: CaptureConfiguration,
    pub extended_configuration: Option<ExtendedConfiguration>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub struct StreamStatus {
    pub id: uuid::Uuid,
    pub running: bool,
    pub error: Option<String>,
    pub video_and_stream: VideoAndStreamInformation,
    pub mavlink: Option<MavlinkComponent>,
}

impl StreamStatus {
    pub fn new(
        id: uuid::Uuid,
        running: bool,
        error: Option<String>,
        video_and_stream: VideoAndStreamInformation,
        mavlink: Option<MavlinkComponent>,
    ) -> Self {
        Self {
            id,
            running,
            error,
            video_and_stream,
            mavlink,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct MavlinkComponent {
    pub system_id: u8,
    pub component_id: u8,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct VideoAndStreamInformation {
    pub name: String,
    pub stream_information: StreamInformation,
    pub video_source: VideoSourceType,
}
