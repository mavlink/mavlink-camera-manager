use anyhow::Result;
use serde::{Deserialize, Serialize};

#[allow(non_snake_case)]
#[path = "../../../target/flatbuffers/mod.rs"]
mod flatbuffer_messages;

use flatbuffer_messages::foxglove;

// https://docs.foxglove.dev/docs/visualization/message-schemas/compressed-video
#[derive(Debug, Serialize, Deserialize)]
pub struct CompressedVideo {
    pub timestamp: Time,
    pub frame_id: String,
    pub data: Bytes,
    pub format: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct Time {
    pub sec: u32,
    pub nsec: u32,
}

impl Default for Time {
    fn default() -> Self {
        let time = chrono::Utc::now();

        Self {
            sec: time.timestamp() as u32, // note: this will wrap if timestamp exceeds u32::MAX
            nsec: time.timestamp_subsec_nanos(),
        }
    }
}

impl From<Time> for foxglove::Time {
    fn from(value: Time) -> Self {
        Self::new(value.sec, value.nsec)
    }
}

pub type Bytes = Vec<u8>;

impl CompressedVideo {
    pub fn to_cbor(&self) -> Result<Vec<u8>> {
        serde_cbor::to_vec(self).map_err(anyhow::Error::msg)
    }

    pub fn to_flatbuffer(&self) -> Result<Vec<u8>> {
        let mut builder = flatbuffers::FlatBufferBuilder::with_capacity(1024);

        let timestamp = &self.timestamp.into();
        let frame_id = builder.create_string(&self.frame_id);
        let data = builder.create_vector(&self.data);
        let format = builder.create_string(&self.format);

        let compressed_video = foxglove::CompressedVideo::create(
            &mut builder,
            &foxglove::CompressedVideoArgs {
                timestamp: Some(timestamp),
                frame_id: Some(frame_id),
                data: Some(data),
                format: Some(format),
            },
        );

        builder.finish(compressed_video, None);

        Ok(builder.finished_data().to_vec())
    }
}
