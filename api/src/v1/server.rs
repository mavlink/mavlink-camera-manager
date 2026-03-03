use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::v1::controls::Control;
use crate::v1::stream::StreamInformation;
use crate::v1::video::Format;

#[derive(Debug, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub struct ApiVideoSource {
    pub name: String,
    pub source: String,
    pub formats: Vec<Format>,
    pub controls: Vec<Control>,
    pub blocked: bool,
}

impl ApiVideoSource {
    pub fn new(
        name: String,
        source: String,
        formats: Vec<Format>,
        controls: Vec<Control>,
        blocked: bool,
    ) -> Self {
        Self {
            name,
            source,
            formats,
            controls,
            blocked,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct V4lControl {
    pub device: String,
    #[ts(type = "number")]
    pub v4l_id: u64,
    #[ts(type = "number")]
    pub value: i64,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct PostStream {
    pub name: String,
    pub source: String,
    pub stream_information: StreamInformation,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct RemoveStream {
    pub name: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct BlockSource {
    pub source_string: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct UnblockSource {
    pub source_string: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ResetSettings {
    pub all: Option<bool>,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ResetCameraControls {
    pub device: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct XmlFileRequest {
    pub file: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct SdpFileRequest {
    pub source: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ThumbnailFileRequest {
    pub source: String,
    /// The Quality level (a percentage value as an integer between 1 and 100) is inversely proportional to JPEG compression level, which means the higher, the best.
    pub quality: Option<u8>,
    /// Target height of the thumbnail. The value should be an integer between 1 and 1080 (because of memory constraints).
    pub target_height: Option<u16>,
}

#[derive(Serialize, Debug, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct Development {
    pub number_of_tasks: usize,
}

#[derive(Serialize, Debug, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
#[non_exhaustive]
pub struct Info {
    /// Name of the program
    pub name: String,
    /// Version/tag
    pub version: String,
    /// Git SHA
    pub sha: String,
    pub build_date: String,
    /// Authors name
    pub authors: String,
    /// Unstable field for custom development
    pub development: Development,
}

impl Info {
    pub fn new(
        name: String,
        version: String,
        sha: String,
        build_date: String,
        authors: String,
        development: Development,
    ) -> Self {
        Self {
            name,
            version,
            sha,
            build_date,
            authors,
            development,
        }
    }
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct AuthenticateOnvifDeviceRequest {
    /// Onvif Device UUID, obtained via `/onvif/devices` get request
    pub device_uuid: uuid::Uuid,
    /// Username for the Onvif Device
    pub username: String,
    /// Password for the Onvif Device
    pub password: String,
}

#[derive(Debug, Deserialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct UnauthenticateOnvifDeviceRequest {
    /// Onvif Device UUID, obtained via `/onvif/devices` get request
    pub device_uuid: uuid::Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct OnvifDevice {
    pub uuid: uuid::Uuid,
    #[ts(type = "string")]
    pub ip: IpAddr,
    pub types: Vec<String>,
    pub hardware: Option<String>,
    pub name: Option<String>,
    pub urls: Vec<url::Url>,
}
