use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use gst::prelude::*;
use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};
use tracing::*;

use crate::{
    controls::types::Control,
    stream::gst::utils::{PluginRequirement, is_gst_plugin_available},
};

use super::{
    types::*,
    video_source::{VideoSource, VideoSourceAvailable, VideoSourceFormats},
    video_source_local::VideoSourceLocal,
};

#[derive(Apiv2Schema, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoSourceGstType {
    // TODO: local should have a pipeline also
    Local(VideoSourceLocal),
    Fake(String),
    QR(String),
    File(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSourceGst {
    pub name: String,
    pub source: VideoSourceGstType,
}

impl VideoSourceFormats for VideoSourceGst {
    async fn formats(&self) -> Vec<Format> {
        match &self.source {
            VideoSourceGstType::Local(local) => local.formats().await,
            VideoSourceGstType::Fake(_) => {
                let intervals: Vec<FrameInterval> = [60, 30, 24, 16, 10, 5]
                    .iter()
                    .map(|&frame_interval| FrameInterval {
                        denominator: frame_interval,
                        numerator: 1,
                    })
                    .collect();

                let sizes: Vec<Size> = [
                    (160, 120),
                    (320, 240),
                    (640, 480),
                    (720, 480),
                    (960, 720),
                    (1280, 720),
                    (1280, 1080),
                    (1440, 1080),
                    (1920, 1080),
                ]
                .iter()
                .map(|&(width, height)| Size {
                    width,
                    height,
                    intervals: intervals.clone(),
                })
                .collect();

                vec![
                    Format {
                        encode: VideoEncodeType::H264,
                        sizes: sizes.clone(),
                    },
                    Format {
                        encode: VideoEncodeType::H265,
                        sizes: sizes.clone(),
                    },
                    Format {
                        encode: VideoEncodeType::Yuyv,
                        sizes: sizes.clone(),
                    },
                    Format {
                        encode: VideoEncodeType::Mjpg,
                        sizes,
                    },
                ]
            }
            VideoSourceGstType::QR(_) => {
                let intervals: Vec<FrameInterval> = [60, 30, 24, 16, 10, 5]
                    .iter()
                    .map(|&frame_interval| FrameInterval {
                        denominator: frame_interval,
                        numerator: 1,
                    })
                    .collect();

                let sizes: Vec<Size> = [
                    (240, 240),
                    (320, 320),
                    (480, 480),
                    (640, 640),
                    (720, 720),
                    (960, 960),
                    (1080, 1080),
                    (1280, 1280),
                    (1440, 1440),
                    (1920, 1920),
                ]
                .iter()
                .map(|&(width, height)| Size {
                    width,
                    height,
                    intervals: intervals.clone(),
                })
                .collect();

                vec![
                    Format {
                        encode: VideoEncodeType::H264,
                        sizes: sizes.clone(),
                    },
                    Format {
                        encode: VideoEncodeType::Rgb,
                        sizes,
                    },
                ]
            }
            VideoSourceGstType::File(path) => match probe_file(path) {
                Ok(format) => vec![format],
                Err(error) => {
                    warn!("Failed to probe file {path:?}: {error:?}");
                    vec![]
                }
            },
        }
    }
}

impl VideoSource for VideoSourceGst {
    fn name(&self) -> &String {
        &self.name
    }

    fn source_string(&self) -> &str {
        match &self.source {
            VideoSourceGstType::Local(local) => local.source_string(),
            VideoSourceGstType::Fake(string) => string,
            VideoSourceGstType::QR(string) => string,
            VideoSourceGstType::File(path) => path,
        }
    }

    fn set_control_by_name(&self, _control_name: &str, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn set_control_by_id(&self, _control_id: u64, _value: i64) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn control_value_by_name(&self, _control_name: &str) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn control_value_by_id(&self, _control_id: u64) -> std::io::Result<i64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Source doesn't have controls.",
        ))
    }

    fn controls(&self) -> Vec<Control> {
        vec![]
    }

    fn is_valid(&self) -> bool {
        match &self.source {
            VideoSourceGstType::Local(local) => local.is_valid(),
            VideoSourceGstType::Fake(string) => match string.as_str() {
                // All valid members are from: https://gstreamer.freedesktop.org/documentation/videotestsrc/index.html?gi-language=c#members-2
                "ball" | "bar" | "black" | "blink" | "blue" | "chroma" | "circular" | "gamut"
                | "gradient" | "green" | "pinwheel" | "red" | "smpte" | "smpte100" | "smpte75"
                | "snow" | "solid" | "spokes" | "white" | "zone" => true,
                _ => false,
            },
            VideoSourceGstType::QR(_) => true,
            VideoSourceGstType::File(path) => Path::new(path).exists(),
        }
    }

    fn is_shareable(&self) -> bool {
        true
    }
}

impl VideoSourceAvailable for VideoSourceGst {
    async fn cameras_available() -> Vec<VideoSourceType> {
        let mut sources = vec![VideoSourceType::Gst(VideoSourceGst {
            name: "Fake source".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        })];
        if is_gst_plugin_available(&PluginRequirement::new("qrtimestampsrc")) {
            sources.push(VideoSourceType::Gst(VideoSourceGst {
                name: "QR".into(),
                source: VideoSourceGstType::QR("QRTimeStamp".into()),
            }));
        };

        let mut paths: Vec<PathBuf> = std::fs::read_dir(file_sources_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| file_demuxer(&path.to_string_lossy()).is_ok())
                    .collect()
            })
            .unwrap_or_default();
        paths.sort();
        sources.extend(paths.into_iter().map(|path| {
            VideoSourceType::Gst(VideoSourceGst {
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                source: VideoSourceGstType::File(path.to_string_lossy().into()),
            })
        }));

        sources
    }
}

/// Folder where the video files used by File sources live
pub fn file_sources_dir() -> PathBuf {
    ProjectDirs::from("com", "Blue Robotics", env!("CARGO_PKG_NAME"))
        .expect("Failed to find user config path.")
        .config_dir()
        .join("videos")
}

pub fn file_demuxer(path: &str) -> Result<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("mp4" | "mov") => Ok("qtdemux"),
        Some("mkv") => Ok("matroskademux"),
        _ => Err(anyhow!(
            "Unsupported container for {path:?}, use mp4, mov or mkv"
        )),
    }
}

#[derive(Default)]
struct FileProbe {
    last_pts: Option<gst::ClockTime>,
    has_b_frames: bool,
}

/// Reads the first frames of the file to get the format of its first H.264 or H.265 video stream
/// and to make sure that it has no B-frames, since they can't be streamed without being re-encoded
pub fn probe_file(path: &str) -> Result<Format> {
    // Two seconds of a 30 fps video are enough to find B-frames
    let description = format!(
        "filesrc name=source ! {} ! capsfilter caps=video/x-h264;video/x-h265 ! fakesink name=sink sync=false num-buffers=60",
        file_demuxer(path)?
    );
    let pipeline = gst::parse::launch(&description)?
        .downcast::<gst::Pipeline>()
        .expect("Couldn't downcast pipeline");
    pipeline
        .by_name("source")
        .context("No filesrc")?
        .set_property("location", path);
    let sink_pad = pipeline
        .by_name("sink")
        .and_then(|sink| sink.static_pad("sink"))
        .context("No fakesink pad")?;

    // B-frames are stored in decode order, so their presentation timestamps go backwards
    let probe = Arc::new(Mutex::new(FileProbe::default()));
    sink_pad.add_probe(gst::PadProbeType::BUFFER, {
        let probe = probe.clone();
        move |_pad, info| {
            if let Some(pts) = info.buffer().and_then(|buffer| buffer.pts()) {
                let mut probe = probe.lock().unwrap();
                if probe.last_pts.is_some_and(|last_pts| pts < last_pts) {
                    probe.has_b_frames = true;
                }
                probe.last_pts = Some(pts);
            }
            gst::PadProbeReturn::Ok
        }
    });

    let result = pipeline.set_state(gst::State::Playing).map(|_| {
        pipeline.bus().and_then(|bus| {
            bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
        })
    });
    let caps = sink_pad.current_caps();
    let _ = pipeline.set_state(gst::State::Null);

    match result {
        Err(error) => return Err(anyhow!("Failed to play {path:?}: {error:?}")),
        Ok(Some(message)) => {
            if let gst::MessageView::Error(error) = message.view() {
                return Err(anyhow!("Failed to read {path:?}: {}", error.error()));
            }
        }
        Ok(None) => {}
    }

    let caps = caps.context("No H.264 or H.265 video stream found")?;

    if probe.lock().unwrap().has_b_frames {
        return Err(anyhow!(
            "Video has B-frames, which can't be streamed. Re-encode it without B-frames, e.g.: ffmpeg -i input.mp4 -c:v libx264 -bf 0 output.mp4"
        ));
    }
    let structure = caps.structure(0).context("Empty caps")?;

    let encode = match structure.name().as_str() {
        "video/x-h264" => VideoEncodeType::H264,
        "video/x-h265" => VideoEncodeType::H265,
        unsupported => return Err(anyhow!("Unsupported encode {unsupported:?}")),
    };
    let framerate = structure.get::<gst::Fraction>("framerate")?;

    Ok(Format {
        encode,
        sizes: vec![Size {
            width: structure.get::<i32>("width")? as u32,
            height: structure.get::<i32>("height")? as u32,
            intervals: vec![FrameInterval {
                numerator: framerate.denom() as u32,
                denominator: framerate.numer() as u32,
            }],
        }],
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn encode_file(path: &str, bframes: u32) {
        let encoder = gst::parse::launch(&format!(
            "videotestsrc num-buffers=30 ! video/x-raw,width=320,height=240,framerate=30/1 ! x264enc bframes={bframes} ! mp4mux ! filesink location={path}"
        ))
        .unwrap();
        encoder.set_state(gst::State::Playing).unwrap();
        encoder.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        encoder.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn probe_file_rejects_b_frames() {
        gst::init().unwrap();

        let folder = tempfile::tempdir().unwrap();
        let path = |name: &str| folder.path().join(name).to_string_lossy().to_string();

        encode_file(&path("without.mp4"), 0);
        let format = probe_file(&path("without.mp4")).unwrap();
        assert_eq!(format.encode, VideoEncodeType::H264);

        encode_file(&path("with.mp4"), 2);
        let error = probe_file(&path("with.mp4")).unwrap_err();
        assert!(error.to_string().contains("B-frames"), "{error:?}");
    }
}
