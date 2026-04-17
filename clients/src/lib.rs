pub mod protocol;
pub mod recording_client;
pub mod rtsp_client;
pub mod udp_client;
pub mod webrtc_client;
pub mod zenoh_client;

pub mod thumbnail_client;

use std::{
    hash::{DefaultHasher, Hasher},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Result;
use gst::prelude::*;
use tokio::sync::mpsc;

#[async_trait::async_trait]
pub trait StreamClient {
    fn frames(&self) -> u64;
    fn pipeline(&self) -> &gst::Pipeline;

    async fn wait_for_frames(&self, min: u64, timeout: Duration) -> Result<u64> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let n = self.frames();
            if n >= min {
                return Ok(n);
            }
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!("only got {n} frames, wanted {min}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_for_continuous_frames(
        &self,
        duration: Duration,
        check_interval: Duration,
    ) -> Result<u64> {
        let deadline = tokio::time::Instant::now() + duration;
        let mut last_count = self.frames();
        let mut stall_start: Option<tokio::time::Instant> = None;
        let max_stall = Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(check_interval).await;
            let now_count = self.frames();
            if now_count > last_count {
                stall_start = None;
                last_count = now_count;
            } else {
                let stall = stall_start.get_or_insert(tokio::time::Instant::now());
                if stall.elapsed() > max_stall {
                    anyhow::bail!(
                        "frame flow stalled at {now_count} frames for {:?}",
                        stall.elapsed()
                    );
                }
            }
        }
        Ok(self.frames())
    }
}

pub struct FrameSample {
    pub content_hash: u64,
    pub relative_pts_ms: i64,
    pub arrival: Instant,
    pub buffer_size: usize,
}

pub type SampleSender = mpsc::UnboundedSender<FrameSample>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    Mjpg,
    Yuyv,
    Rgb,
}

/// Hash only VCL NAL units from an H.264/H.265 byte-stream buffer.
/// This produces a stable hash across different pipeline processing chains
/// (SPS/PPS injection, stream-format conversion, etc.) because the actual
/// coded slice data is never modified by parse/pay/depay elements.
pub fn hash_vcl_nals(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut vcl_bytes = 0usize;
    let mut i = 0;
    while i < data.len() {
        let (sc_len, nal_start) = if i + 3 < data.len() && data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                (3, i + 3)
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                (4, i + 4)
            } else {
                i += 1;
                continue;
            }
        } else {
            i += 1;
            continue;
        };

        if nal_start >= data.len() {
            break;
        }

        let mut nal_end = data.len();
        for j in nal_start..data.len().saturating_sub(2) {
            if data[j] == 0
                && data[j + 1] == 0
                && (data[j + 2] == 1
                    || (j + 3 < data.len() && data[j + 2] == 0 && data[j + 3] == 1))
            {
                nal_end = j;
                break;
            }
        }

        let nal_type = data[nal_start] & 0x1F;
        // VCL NAL types: 1-5 (non-IDR slice, partition A/B/C, IDR slice)
        if (1..=5).contains(&nal_type) {
            hasher.write(&data[nal_start..nal_end]);
            vcl_bytes += nal_end - nal_start;
        }

        i = if nal_end > nal_start + sc_len {
            nal_end
        } else {
            nal_start + 1
        };
    }

    if vcl_bytes == 0 {
        hasher.write(data);
    }

    hasher.finish()
}

/// Attach a pad probe that hashes each buffer's VCL NAL content and records
/// the hash together with (relative_pts_ms, wall-clock Instant). Matching by
/// VCL content hash works across different processing chains (depay/parse/pay)
/// because the coded slice data passes through unchanged.
pub fn attach_frame_probe(pad: &gst::Pad, client_name: String, sender: SampleSender) {
    let first_pts: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));

    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        let Some(gst::PadProbeData::Buffer(ref buffer)) = info.data else {
            return gst::PadProbeReturn::Ok;
        };

        let arrival = Instant::now();

        let Ok(map) = buffer.map_readable() else {
            return gst::PadProbeReturn::Ok;
        };
        let buffer_size = map.len();
        let content_hash = hash_vcl_nals(map.as_slice());

        let relative_pts_ms = buffer.pts().map_or(-1, |pts| {
            let pts_ns = pts.nseconds();
            let mut first = first_pts.lock().unwrap();
            let base = *first.get_or_insert(pts_ns);
            ((pts_ns - base) / 1_000_000) as i64
        });

        if sender
            .send(FrameSample {
                content_hash,
                relative_pts_ms,
                arrival,
                buffer_size,
            })
            .is_err()
        {
            eprintln!("[{client_name}] Sample channel closed");
        }

        gst::PadProbeReturn::Ok
    });
}
