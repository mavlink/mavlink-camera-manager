use crate::{
    video::types::{FrameInterval, VideoEncodeType},
    video_stream::types::VideoAndStreamInformation,
};

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct VideoCaptureConfiguration {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
}

#[deprecated(note = "The API will soon allow for optional CaptureConfiguration instead")]
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct RedirectCaptureConfiguration {}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CaptureConfiguration {
    Video(VideoCaptureConfiguration),
    /// This is only still used for easy stream creation, and it is always converted to Self::Video.
    Redirect(RedirectCaptureConfiguration),
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
pub struct ExtendedConfiguration {
    pub thermal: bool,
    pub disable_mavlink: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Apiv2Schema)]
pub struct StreamInformation {
    pub endpoints: Endpoints,
    pub configuration: CaptureConfiguration,
    pub extended_configuration: Option<ExtendedConfiguration>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Apiv2Schema)]
#[serde(untagged)]
pub enum Endpoints {
    Array(Vec<Url>),
    Object {
        origin: Option<Url>,
        destination: Vec<Url>,
    },
}

impl std::ops::Deref for Endpoints {
    type Target = Vec<Url>;

    fn deref(&self) -> &Self::Target {
        match self {
            Endpoints::Array(array) => &array,
            Endpoints::Object {
                origin: _,
                destination,
            } => &destination,
        }
    }
}

impl From<Vec<Url>> for Endpoints {
    fn from(destination: Vec<Url>) -> Self {
        Endpoints::Object {
            origin: None,
            destination,
        }
    }
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct StreamStatus {
    pub id: uuid::Uuid,
    pub running: bool,
    pub error: Option<String>,
    pub video_and_stream: VideoAndStreamInformation,
    pub mavlink: Option<MavlinkComponent>,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct MavlinkComponent {
    pub system_id: u8,
    pub component_id: u8,
}
