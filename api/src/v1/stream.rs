use serde::{Deserialize, Serialize};
use ts_rs::TS;
use url::Url;

use crate::v1::video::{FrameInterval, VideoEncodeType, VideoSourceType};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
pub struct VideoCaptureConfiguration {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
}

#[deprecated(note = "The API will soon allow for optional CaptureConfiguration instead")]
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, TS)]
pub struct RedirectCaptureConfiguration {}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CaptureConfiguration {
    Video(VideoCaptureConfiguration),
    /// This is only still used for easy stream creation, and it is always converted to Self::Video.
    #[allow(deprecated)]
    Redirect(RedirectCaptureConfiguration),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(default)]
pub struct ExtendedConfiguration {
    pub thermal: bool,
    pub disable_mavlink: bool,
    pub disable_zenoh: bool,
    pub disable_thumbnails: bool,
    pub disable_lazy: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub configuration: CaptureConfiguration,
    pub extended_configuration: Option<ExtendedConfiguration>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[serde(rename_all = "lowercase")]
pub enum StreamStatusState {
    Running,
    Idle,
    Stopped,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct StreamStatus {
    pub id: uuid::Uuid,
    pub running: bool,
    pub state: StreamStatusState,
    pub error: Option<String>,
    pub video_and_stream: VideoAndStreamInformation,
    pub mavlink: Option<MavlinkComponent>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
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
