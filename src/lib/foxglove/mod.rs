use anyhow::{Context, Result};
use foxglove::Encode;
use serde::{Deserialize, Serialize};

#[allow(non_snake_case)]
#[path = "../../../target/flatbuffers/mod.rs"]
mod flatbuffer_messages;

pub mod ros2_messages {
    #[rustfmt::skip]
    rosrust::rosmsg_include!(foxglove_msgs/CompressedVideo);
}

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
    pub sec: i32,
    pub nsec: u32,
}

impl Default for Time {
    fn default() -> Self {
        let time = chrono::Utc::now();

        Self {
            sec: time.timestamp() as i32, // note: this will wrap if timestamp exceeds u32::MAX
            nsec: time.timestamp_subsec_nanos(),
        }
    }
}

pub type Bytes = Vec<u8>;

impl CompressedVideo {
    pub fn to_cbor(&self) -> Result<Vec<u8>> {
        serde_cbor::to_vec(self).map_err(anyhow::Error::msg)
    }

    pub fn to_flatbuffer(&self) -> Result<Vec<u8>> {
        use flatbuffer_messages::foxglove;

        let mut builder = flatbuffers::FlatBufferBuilder::with_capacity(1024);

        let timestamp =
            &foxglove::Time::new(self.timestamp.sec.try_into().unwrap(), self.timestamp.nsec);
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

    pub fn to_ros2(&self) -> Result<Vec<u8>> {
        use ros2_messages::{builtin_interfaces, foxglove_msgs};
        use rosrust::RosMsg;

        let message = foxglove_msgs::CompressedVideo {
            timestamp: builtin_interfaces::Time {
                sec: self.timestamp.sec,
                nanosec: self.timestamp.nsec,
            },
            frame_id: self.frame_id.clone(),
            data: self.data.clone(),
            format: self.format.clone(),
        };

        message.encode_vec().map_err(anyhow::Error::msg)
    }

    pub fn to_cdr(&self) -> Result<Vec<u8>> {
        cdr::serialize::<_, _, cdr::PlCdrLe>(&self, cdr::Infinite).map_err(anyhow::Error::msg)
    }

    pub fn to_protobuf(&self) -> Result<Vec<u8>> {
        use foxglove;

        let data = foxglove::schemas::CompressedVideo {
            timestamp: Some(foxglove::schemas::Timestamp::new(
                self.timestamp.sec as u32,
                self.timestamp.nsec,
            )),
            frame_id: self.frame_id.clone(),
            data: self.data.clone().into(),
            format: self.format.clone(),
        };

        let len = data.encoded_len().context("Unknown encoded size")?;
        let mut buf: Vec<u8> = Vec::with_capacity(len);
        data.encode(&mut buf)?;

        Ok(buf)
    }
}
