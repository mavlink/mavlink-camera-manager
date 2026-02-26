use serde::Serialize;

/// A timestamp, represented as an offset from a user-defined epoch.
/// <https://docs.foxglove.dev/docs/visualization/message-schemas/built-in-types#time>
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Timestamp {
    /// Seconds since epoch.
    sec: u32,
    /// Additional nanoseconds since epoch.
    nsec: u32,
}

impl Timestamp {
    pub fn new(sec: u32, nsec: u32) -> Self {
        Self { sec, nsec }
    }

    pub fn now() -> Self {
        let now = chrono::Utc::now();
        Self {
            sec: now.timestamp() as u32,
            nsec: now.timestamp_subsec_nanos(),
        }
    }
}

/// A single frame of a compressed video bitstream
/// <https://docs.foxglove.dev/docs/visualization/message-schemas/compressed-video>
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct CompressedVideo {
    /// Timestamp of video frame
    pub timestamp: Timestamp,
    /// Frame of reference for the video.
    ///
    /// The origin of the frame is the optical center of the camera. +x points to the right in the video, +y points down, and +z points into the plane of the video.
    pub frame_id: String,
    /// Compressed video frame data.
    ///
    /// For packet-based video codecs this data must begin and end on packet boundaries (no partial packets), and must contain enough video packets to decode exactly one image (either a keyframe or delta frame). Note: Foxglove does not support video streams that include B frames because they require lookahead.
    ///
    /// Specifically, the requirements for different `format` values are:
    ///
    /// - `h264`
    ///    - Use Annex B formatted data
    ///    - Each CompressedVideo message should contain enough NAL units to decode exactly one video frame
    ///    - Each message containing a key frame (IDR) must also include a SPS NAL unit
    ///
    /// - `h265` (HEVC)
    ///    - Use Annex B formatted data
    ///    - Each CompressedVideo message should contain enough NAL units to decode exactly one video frame
    ///    - Each message containing a key frame (IRAP) must also include relevant VPS/SPS/PPS NAL units
    ///
    /// - `vp9`
    ///    - Each CompressedVideo message should contain exactly one video frame
    ///
    /// - `av1`
    ///    - Use the "Low overhead bitstream format" (section 5.2)
    ///    - Each CompressedVideo message should contain enough OBUs to decode exactly one video frame
    ///    - Each message containing a key frame must also include a Sequence Header OBU
    pub data: Vec<u8>,
    /// Video format.
    ///
    /// Supported values: `h264`, `h265`, `vp9`, `av1`.
    ///
    /// Note: compressed video support is subject to hardware limitations and patent licensing, so not all encodings may be supported on all platforms. See more about [H.265 support](<https://caniuse.com/hevc>), [VP9 support](<https://caniuse.com/webm>), and [AV1 support](<https://caniuse.com/av1>).
    pub format: String,
}

/// A log message
/// <https://docs.foxglove.dev/docs/visualization/message-schemas/log>
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Log {
    /// Timestamp of log message
    pub timestamp: Timestamp,
    /// Log level
    pub level: Level,
    /// Log message
    pub message: String,
    /// Process or node name
    pub name: String,
    /// Filename
    pub file: String,
    /// Line number in the file
    pub line: u32,
}

/// Log level
/// <https://docs.foxglove.dev/docs/sdk/schemas/log-level>
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[repr(i32)]
pub enum Level {
    /// Unknown log level
    Unknown = 0,
    /// Debug log level
    Debug = 1,
    /// Info log level
    Info = 2,
    /// Warning log level
    Warning = 3,
    /// Error log level
    Error = 4,
    /// Fatal log level
    Fatal = 5,
}
