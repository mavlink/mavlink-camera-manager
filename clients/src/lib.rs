pub mod protocol;
pub mod rtsp_client;
pub mod thumbnail_client;
pub mod udp_client;
pub mod webrtc_client;
pub mod zenoh_client;

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
    /// Cumulative buffers seen on the probe pad since connect, not a frame rate.
    fn frames(&self) -> u64;
    fn pipeline(&self) -> &gst::Pipeline;

    async fn wait_for_frames(&self, min: u64, timeout: Duration) -> Result<u64> {
        anyhow::ensure!(min > 0, "min must be positive");
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let count = self.frames();
            if count >= min {
                return Ok(count);
            }
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!("only got {count} frames, wanted {min}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait until `duration` elapses while frames keep arriving at about `fps`.
    ///
    /// `check_interval` is one frame period (`1/fps`). Stall is three seconds of
    /// content at `fps` (90 missed frames at 30 fps), matching the previous
    /// hardcoded 3s budget.
    async fn wait_for_continuous_frames(&self, fps: f64, duration: Duration) -> Result<u64> {
        anyhow::ensure!(fps > 0.0, "fps must be positive");
        let frame_period = Duration::from_secs_f64(1.0 / fps);
        anyhow::ensure!(
            duration > frame_period,
            "duration must exceed one frame period"
        );
        let deadline = tokio::time::Instant::now() + duration;
        let mut last_count = self.frames();
        let mut stall_start: Option<tokio::time::Instant> = None;
        let max_stall = Duration::from_secs_f64(90.0 / fps);
        while tokio::time::Instant::now() < deadline {
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
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            tokio::time::sleep((deadline - now).min(frame_period)).await;
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

/// Hash VCL NAL units so identity is stable across parse/pay/depay.
///
/// Hand-rolled Annex-B scan instead of `rust_h264`/`rust_h265`: this is a
/// test identity hash, not a decoder, and those crates would add a dependency
/// for a start-code walk. H.264 and H.265 NAL type fields differ (5-bit vs
/// 6-bit); sharing `& 0x1F` silently dropped H.265 slices.
pub fn hash_vcl_nals(data: &[u8], codec: Codec) -> u64 {
    match codec {
        Codec::H264 => hash_annex_b_vcl(data, is_h264_vcl),
        Codec::H265 => hash_annex_b_vcl(data, is_h265_vcl),
        Codec::Mjpg | Codec::Yuyv | Codec::Rgb => hash_bytes(data),
    }
}

/// Attach a pad probe that hashes each buffer's VCL NAL content and records
/// the hash together with (relative_pts_ms, wall-clock Instant). Matching by
/// VCL content hash works across different processing chains (depay/parse/pay)
/// because the coded slice data passes through unchanged.
pub fn attach_frame_probe(pad: &gst::Pad, client_name: String, sender: SampleSender, codec: Codec) {
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
        let content_hash = hash_vcl_nals(map.as_slice(), codec);

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

fn is_h264_vcl(header: u8) -> bool {
    (1..=5).contains(&(header & 0x1F))
}

fn is_h265_vcl(header: u8) -> bool {
    let nal_type = (header >> 1) & 0x3F;
    nal_type <= 31
}

fn hash_bytes(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(data);
    hasher.finish()
}

fn hash_annex_b_vcl(data: &[u8], is_vcl: fn(u8) -> bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut vcl_bytes = 0usize;
    let mut index = 0;
    while index < data.len() {
        let (start_code_len, nal_start) =
            if index + 3 < data.len() && data[index] == 0 && data[index + 1] == 0 {
                if data[index + 2] == 1 {
                    (3, index + 3)
                } else if index + 4 <= data.len() && data[index + 2] == 0 && data[index + 3] == 1 {
                    (4, index + 4)
                } else {
                    index += 1;
                    continue;
                }
            } else {
                index += 1;
                continue;
            };

        if nal_start >= data.len() {
            break;
        }

        let mut nal_end = data.len();
        for scan in nal_start..data.len().saturating_sub(2) {
            if data[scan] == 0
                && data[scan + 1] == 0
                && (data[scan + 2] == 1
                    || (scan + 3 < data.len() && data[scan + 2] == 0 && data[scan + 3] == 1))
            {
                nal_end = scan;
                break;
            }
        }

        if is_vcl(data[nal_start]) {
            hasher.write(&data[nal_start..nal_end]);
            vcl_bytes += nal_end - nal_start;
        }

        index = if nal_end > nal_start + start_code_len {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_h264_idr_ignores_sps() {
        let mut buffer = vec![0, 0, 0, 1, 0x67, b's', b'p', b's'];
        buffer.extend_from_slice(&[0, 0, 0, 1, 0x65, b'i', b'd', b'r']);
        let vcl_only = hash_vcl_nals(&[0x65, b'i', b'd', b'r'], Codec::H264);
        assert_eq!(hash_vcl_nals(&buffer, Codec::H264), vcl_only);
    }

    #[test]
    fn hash_h265_idr_uses_six_bit_nal_type() {
        // HEVC IDR_W_RADL is nal_type 19; first byte is (19 << 1) = 0x26.
        // The H.264 5-bit mask would read type 6 and skip this NAL.
        let buffer = [0, 0, 0, 1, 0x26, b's', b'l', b'i', b'c', b'e'];
        let h265 = hash_vcl_nals(&buffer, Codec::H265);
        let h264 = hash_vcl_nals(&buffer, Codec::H264);
        assert_ne!(h265, h264);
        assert_eq!(
            h265,
            hash_vcl_nals(&[0x26, b's', b'l', b'i', b'c', b'e'], Codec::H265)
        );
    }
}
