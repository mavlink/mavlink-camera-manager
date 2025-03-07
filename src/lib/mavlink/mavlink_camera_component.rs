use anyhow::Result;
use tracing::*;

use crate::video_stream::types::VideoAndStreamInformation;

#[derive(Debug, Clone)]
pub struct MavlinkCameraComponent {
    // MAVLink specific information
    pub system_id: u8,
    pub component_id: u8,
    pub stream_id: u8,

    pub vendor_name: String,
    pub model_name: String,
    pub firmware_version: u32,
    pub resolution_h: u16,
    pub resolution_v: u16,
    pub framerate: f32,
    pub bitrate: u32,
    pub rotation: u16,
    pub hfov: u16,
    pub thermal: bool,
}

impl MavlinkCameraComponent {
    #[instrument(level = "debug")]
    pub fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        component_id: u8,
    ) -> Result<Self> {
        let (resolution_h, resolution_v, framerate) = match &video_and_stream_information
            .stream_information
            .configuration
        {
            crate::stream::types::CaptureConfiguration::Video(cfg) => {
                let framerate =
                    cfg.frame_interval.denominator as f32 / cfg.frame_interval.numerator as f32;
                (cfg.height as u16, cfg.width as u16, framerate)
            }
            crate::stream::types::CaptureConfiguration::Redirect(_) => {
                unreachable!("Redirect streams now use CaptureConfiguration::Video")
            }
        };

        let thermal = video_and_stream_information
            .stream_information
            .extended_configuration
            .clone()
            .unwrap_or_default()
            .thermal;

        Ok(Self {
            system_id: crate::cli::manager::mavlink_system_id(),
            component_id,
            stream_id: 1, // Starts at 1, 0 is for broadcast.

            vendor_name: video_and_stream_information
                .video_source
                .inner()
                .name()
                .to_string(),
            model_name: video_and_stream_information.name.clone(),
            firmware_version: 0,
            resolution_h,
            resolution_v,
            bitrate: 5000,
            rotation: 0,
            hfov: 90,
            framerate,
            thermal,
        })
    }

    pub fn header(&self, sequence: Option<u8>) -> mavlink::MavHeader {
        mavlink::MavHeader {
            system_id: self.system_id,
            component_id: self.component_id,
            sequence: sequence.unwrap_or(1),
        }
    }
}
