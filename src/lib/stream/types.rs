use std::collections::BTreeMap;

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use url::Url;

use crate::{
    video::types::{FrameInterval, VideoEncodeType},
    video_stream::types::VideoAndStreamInformation,
};

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Bool(bool),
    Integer(i64),
    Number(f64),
    String(String),
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ManualTranscodingConfig {
    pub encoder: String,
    #[serde(default)]
    pub encoder_properties: BTreeMap<String, PropertyValue>,
    #[serde(default)]
    pub decoder: String,
    #[serde(default)]
    pub decoder_properties: BTreeMap<String, PropertyValue>,
}

/// Autobin picks decoder/encoder internally; property bags persist user values by codec_role.
#[derive(Apiv2Schema, Clone, Debug, PartialEq, Default, Deserialize, Serialize)]
pub struct AutoTranscodingConfig {
    #[serde(default)]
    pub encoder_properties: BTreeMap<String, PropertyValue>,
    #[serde(default)]
    pub decoder_properties: BTreeMap<String, PropertyValue>,
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfiguration {
    #[default]
    Classic,
    #[serde(rename = "auto")]
    AutoTranscoding(AutoTranscodingConfig),
    #[serde(rename = "manual")]
    ManualTranscoding(ManualTranscodingConfig),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoCaptureConfiguration {
    pub source_encode: VideoEncodeType,
    pub sink_encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
    pub frame_interval: FrameInterval,
    pub bit_depth: Option<u32>,
    pub source_configuration: SourceConfiguration,
    pub auto_restart_on_config_change: bool,
}

#[derive(Deserialize)]
struct VideoCaptureConfigurationSerde {
    #[serde(default)]
    source_encode: Option<VideoEncodeType>,
    #[serde(default, rename = "encode")]
    sink_encode: Option<VideoEncodeType>,
    height: u32,
    width: u32,
    frame_interval: FrameInterval,
    #[serde(default)]
    bit_depth: Option<u32>,
    #[serde(default)]
    source_configuration: Option<SourceConfiguration>,
    #[serde(default)]
    auto_restart_on_config_change: bool,
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
#[serde(default)]
pub struct ExtendedConfiguration {
    pub thermal: bool,
    pub disable_mavlink: bool,
    pub disable_zenoh: bool,
    pub disable_thumbnails: bool,
    pub disable_lazy: bool,
    pub disable_recording: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Apiv2Schema)]
pub struct StreamInformation {
    pub endpoints: Vec<Url>,
    pub configuration: CaptureConfiguration,
    pub extended_configuration: Option<ExtendedConfiguration>,
}

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamStatusState {
    Running,
    Idle,
    Stopped,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct StreamStatus {
    pub id: uuid::Uuid,
    pub running: bool,
    pub state: StreamStatusState,
    #[serde(default)]
    pub restart_needed: bool,
    pub error: Option<String>,
    pub video_and_stream: VideoAndStreamInformation,
    pub mavlink: Option<MavlinkComponent>,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct MavlinkComponent {
    pub system_id: u8,
    pub component_id: u8,
}

impl VideoCaptureConfiguration {
    pub fn apply_probe_result(&mut self, probed: &Self) {
        if self.source_encode == self.sink_encode {
            self.source_encode = probed.source_encode.clone();
            self.sink_encode = probed.sink_encode.clone();
        } else {
            self.source_encode = probed.source_encode.clone();
        }
        self.height = probed.height;
        self.width = probed.width;
        self.frame_interval = probed.frame_interval.clone();
    }

    pub fn normalize_source_configuration(&mut self) {
        if self.source_encode != self.sink_encode
            && matches!(self.source_configuration, SourceConfiguration::Classic)
        {
            self.source_configuration =
                SourceConfiguration::AutoTranscoding(AutoTranscodingConfig::default());
        }
    }
}

impl Serialize for VideoCaptureConfiguration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let field_count = 4
            + usize::from(self.source_encode != self.sink_encode)
            + usize::from(self.bit_depth.is_some())
            + usize::from(!matches!(
                self.source_configuration,
                SourceConfiguration::Classic
            ))
            + usize::from(self.auto_restart_on_config_change);

        let mut state = serializer.serialize_struct("VideoCaptureConfiguration", field_count)?;

        if self.source_encode != self.sink_encode {
            state.serialize_field("source_encode", &self.source_encode)?;
        }
        state.serialize_field("encode", &self.sink_encode)?;
        state.serialize_field("height", &self.height)?;
        state.serialize_field("width", &self.width)?;
        state.serialize_field("frame_interval", &self.frame_interval)?;
        if let Some(bit_depth) = self.bit_depth {
            state.serialize_field("bit_depth", &bit_depth)?;
        }
        if !matches!(self.source_configuration, SourceConfiguration::Classic) {
            state.serialize_field("source_configuration", &self.source_configuration)?;
        }
        if self.auto_restart_on_config_change {
            state.serialize_field(
                "auto_restart_on_config_change",
                &self.auto_restart_on_config_change,
            )?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for VideoCaptureConfiguration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = VideoCaptureConfigurationSerde::deserialize(deserializer)?;

        let (source_encode, sink_encode) = match (raw.source_encode, raw.sink_encode) {
            (Some(source_encode), Some(sink_encode)) => (source_encode, sink_encode),
            (None, Some(sink_encode)) => (sink_encode.clone(), sink_encode),
            (Some(source_encode), None) => (source_encode.clone(), source_encode),
            (None, None) => {
                return Err(de::Error::custom("missing encode field"));
            }
        };

        let source_configuration = match raw.source_configuration {
            Some(source_configuration) => source_configuration,
            None if source_encode == sink_encode => SourceConfiguration::Classic,
            None => SourceConfiguration::AutoTranscoding(AutoTranscodingConfig::default()),
        };

        Ok(VideoCaptureConfiguration {
            source_encode,
            sink_encode,
            height: raw.height,
            width: raw.width,
            frame_interval: raw.frame_interval,
            bit_depth: raw.bit_depth,
            source_configuration,
            auto_restart_on_config_change: raw.auto_restart_on_config_change,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn legacy_json_encode_only_deserializes_as_classic_h264() {
        let configuration: VideoCaptureConfiguration = serde_json::from_value(json!({
            "encode": "H264",
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect("legacy JSON should deserialize");

        assert_eq!(configuration.source_encode, VideoEncodeType::H264);
        assert_eq!(configuration.sink_encode, VideoEncodeType::H264);
        assert_eq!(
            configuration.source_configuration,
            SourceConfiguration::Classic
        );

        let serialized = serde_json::to_value(&configuration).expect("serialize");
        assert_eq!(serialized["encode"], Value::String("H264".to_string()));
        assert!(serialized.get("source_encode").is_none());
        assert!(serialized.get("source_configuration").is_none());
    }

    #[test]
    fn legacy_raw_encode_nv12_deserializes_as_classic() {
        let configuration: VideoCaptureConfiguration = serde_json::from_value(json!({
            "encode": "NV12",
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect("legacy NV12 JSON should deserialize");

        assert_eq!(configuration.source_encode, VideoEncodeType::Nv12);
        assert_eq!(configuration.sink_encode, VideoEncodeType::Nv12);
        assert_eq!(
            configuration.source_configuration,
            SourceConfiguration::Classic
        );
    }

    #[test]
    fn transcoding_json_without_source_configuration_deserializes_to_auto_transcoding() {
        let configuration: VideoCaptureConfiguration = serde_json::from_value(json!({
            "source_encode": "NV12",
            "encode": "H264",
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect("transcoding JSON should deserialize");

        assert_eq!(configuration.source_encode, VideoEncodeType::Nv12);
        assert_eq!(configuration.sink_encode, VideoEncodeType::H264);
        assert_eq!(
            configuration.source_configuration,
            SourceConfiguration::AutoTranscoding(AutoTranscodingConfig::default())
        );

        let serialized = serde_json::to_value(&configuration).expect("serialize");
        assert_eq!(
            serialized["source_encode"],
            Value::String("NV12".to_string())
        );
        assert_eq!(serialized["encode"], Value::String("H264".to_string()));
        assert_eq!(serialized["source_configuration"]["type"], "auto");
    }

    #[test]
    fn explicit_manual_transcoding_still_deserializes() {
        let configuration: VideoCaptureConfiguration = serde_json::from_value(json!({
            "source_encode": "NV12",
            "encode": "H264",
            "source_configuration": {
                "type": "manual",
                "encoder": "x264enc",
                "decoder": "nv12decoder",
            },
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect("explicit manual JSON should deserialize");

        assert_eq!(
            configuration.source_configuration,
            SourceConfiguration::ManualTranscoding(ManualTranscodingConfig {
                encoder: "x264enc".to_string(),
                encoder_properties: BTreeMap::new(),
                decoder: "nv12decoder".to_string(),
                decoder_properties: BTreeMap::new(),
            })
        );
    }

    #[test]
    fn normalize_upgrades_classic_mismatched_encodes_to_auto_transcoding() {
        let mut configuration = VideoCaptureConfiguration {
            source_encode: VideoEncodeType::Nv12,
            sink_encode: VideoEncodeType::H264,
            height: 720,
            width: 1280,
            frame_interval: FrameInterval {
                numerator: 1,
                denominator: 30,
            },
            bit_depth: None,
            source_configuration: SourceConfiguration::Classic,
            auto_restart_on_config_change: false,
        };

        configuration.normalize_source_configuration();

        assert_eq!(
            configuration.source_configuration,
            SourceConfiguration::AutoTranscoding(AutoTranscodingConfig::default())
        );
    }

    #[test]
    fn explicit_classic_with_mismatched_encodes_still_deserializes() {
        let configuration: VideoCaptureConfiguration = serde_json::from_value(json!({
            "source_encode": "NV12",
            "encode": "H264",
            "source_configuration": { "type": "classic" },
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect("explicit Classic with mismatched encodes should deserialize");

        assert_eq!(configuration.source_encode, VideoEncodeType::Nv12);
        assert_eq!(configuration.sink_encode, VideoEncodeType::H264);
        assert_eq!(
            configuration.source_configuration,
            SourceConfiguration::Classic
        );
    }

    #[test]
    fn auto_restart_on_config_change_skipped_when_false() {
        let configuration = VideoCaptureConfiguration {
            source_encode: VideoEncodeType::H264,
            sink_encode: VideoEncodeType::H264,
            height: 720,
            width: 1280,
            frame_interval: FrameInterval {
                numerator: 1,
                denominator: 30,
            },
            bit_depth: None,
            source_configuration: SourceConfiguration::Classic,
            auto_restart_on_config_change: false,
        };

        let serialized = serde_json::to_value(&configuration).expect("serialize");
        assert!(serialized.get("auto_restart_on_config_change").is_none());

        let configuration = VideoCaptureConfiguration {
            auto_restart_on_config_change: true,
            ..configuration
        };
        let serialized = serde_json::to_value(&configuration).expect("serialize");
        assert_eq!(
            serialized["auto_restart_on_config_change"],
            Value::Bool(true)
        );
    }

    #[test]
    fn auto_transcoding_config_roundtrips_codec_properties() {
        let configuration: VideoCaptureConfiguration = serde_json::from_value(json!({
            "source_encode": "NV12",
            "encode": "H264",
            "source_configuration": {
                "type": "auto",
                "encoder_properties": { "bitrate": 4000 },
                "decoder_properties": {}
            },
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect("auto transcoding JSON with properties should deserialize");

        let SourceConfiguration::AutoTranscoding(auto_config) = &configuration.source_configuration
        else {
            panic!("expected auto transcoding");
        };
        assert_eq!(
            auto_config.encoder_properties.get("bitrate"),
            Some(&PropertyValue::Integer(4000))
        );

        let serialized = serde_json::to_value(&configuration).expect("serialize");
        assert_eq!(
            serialized["source_configuration"]["encoder_properties"]["bitrate"],
            Value::Number(4000.into())
        );
    }

    #[test]
    fn missing_encode_and_source_encode_errors() {
        let error = serde_json::from_value::<VideoCaptureConfiguration>(json!({
            "width": 1280,
            "height": 720,
            "frame_interval": { "numerator": 1, "denominator": 30 },
        }))
        .expect_err("missing encode fields should error");

        assert!(error.to_string().contains("missing encode field"));
    }
}
