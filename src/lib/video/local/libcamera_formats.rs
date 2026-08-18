//! Native libcamera format / mode enumeration (rpicam-apps style).
//!
//! GStreamer's `libcamerasrc` device-monitor caps invent a large STANDARD_SIZES
//! grid and omit framerates. With the `libcamera-native` feature this module
//! lists ISP-processed NV12 / RGB / YUYV at **discrete sensor sizes** from
//! `StreamRole::Raw`, configures each mode via `StreamRole::VideoRecording`,
//! and reads `FrameDurationLimits` for a discrete FPS list (common rates up to
//! the mode max — the ISP accepts any duration in that range, not only max).
//!
//! VideoRecording alone advertises dozens of ISP-scalable sizes (e.g. 2560×1080)
//! that are not sensor modes and often produce gray / low-FPS streams. Restricting
//! to Raw WxH keeps the UI on real modes (e.g. imx219 3280×2464 / 1640×1232).
//!
//! Bayer / Raw formats are not listed: software demosaic is far more expensive
//! than the ISP path, and packing mismatches break `bayer2rgb` on Pi.
//!
//! When an in-process CameraManager cannot start (e.g. GStreamer's libcamera
//! provider already owns one), we re-exec ourselves with [`DUMP_ENV`] set so a
//! child process (no GStreamer init) can dump JSON.

use std::io;

use tracing::*;

use crate::video::types::Format;

/// Env var: when set to a camera id, `main` dumps formats as JSON and exits.
pub const DUMP_ENV: &str = "MCM_DUMP_LIBCAMERA_FORMATS";

/// List native formats for `camera_name` (libcamera camera id / device path).
#[instrument(level = "debug")]
pub fn list_formats(camera_name: &str) -> io::Result<Vec<Format>> {
    #[cfg(feature = "libcamera-native")]
    {
        native::list_formats(camera_name)
    }
    #[cfg(not(feature = "libcamera-native"))]
    {
        let _ = camera_name;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "built without libcamera-native feature; use GStreamer caps fallback",
        ))
    }
}

/// Dump formats as JSON to stdout (used by the `DUMP_ENV` child process).
pub fn dump_to_stdout(camera_name: &str) -> io::Result<()> {
    #[cfg(feature = "libcamera-native")]
    {
        native::dump_to_stdout(camera_name)
    }
    #[cfg(not(feature = "libcamera-native"))]
    {
        let _ = camera_name;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "built without libcamera-native feature",
        ))
    }
}

#[cfg(feature = "libcamera-native")]
mod native {
    use std::{
        collections::{HashMap, HashSet},
        io,
        process::Command,
        str::FromStr,
        sync::Mutex,
    };

    use libcamera::{
        camera_manager::CameraManager,
        control::ControlError,
        controls::{ControlId, FrameDurationLimits},
        geometry::Size as LcSize,
        logging::LoggingLevel,
        stream::StreamRole,
    };
    use tracing::*;

    use super::DUMP_ENV;
    use crate::video::types::{Format, FrameInterval, Size, VideoEncodeType};

    static CACHE: Mutex<Option<(String, Vec<Format>)>> = Mutex::new(None);

    pub fn list_formats(camera_name: &str) -> io::Result<Vec<Format>> {
        if let Ok(cache) = CACHE.lock() {
            if let Some((cached_name, formats)) = cache.as_ref() {
                if cached_name == camera_name {
                    return Ok(formats.clone());
                }
            }
        }

        let formats = match list_formats_in_process(camera_name) {
            Ok(formats) => formats,
            Err(error) => {
                debug!(
                    "In-process libcamera format enum failed for {camera_name:?} ({error}); trying subprocess"
                );
                list_formats_via_subprocess(camera_name)?
            }
        };

        if let Ok(mut cache) = CACHE.lock() {
            *cache = Some((camera_name.to_string(), formats.clone()));
        }

        Ok(formats)
    }

    pub fn dump_to_stdout(camera_name: &str) -> io::Result<()> {
        let formats = list_formats_in_process(camera_name)?;
        let json = serde_json::to_string(&formats).map_err(io::Error::other)?;
        println!("{json}");
        Ok(())
    }

    fn list_formats_via_subprocess(camera_name: &str) -> io::Result<Vec<Format>> {
        let exe = std::env::current_exe()?;
        let output = Command::new(exe)
            .env(DUMP_ENV, camera_name)
            .env_remove("RUST_LOG")
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(io::Error::other(format!(
                "libcamera formats subprocess failed (status {}): {stderr}",
                output.status
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_line = stdout
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('['))
            .ok_or_else(|| io::Error::other("libcamera formats subprocess produced no JSON"))?;

        serde_json::from_str(json_line.trim()).map_err(|error| {
            io::Error::other(format!(
                "Failed parsing libcamera formats subprocess JSON: {error}: {json_line}"
            ))
        })
    }

    fn list_formats_in_process(camera_name: &str) -> io::Result<Vec<Format>> {
        let mgr = CameraManager::new()?;
        let _ = mgr.log_set_level("Camera", LoggingLevel::Error);
        let _ = mgr.log_set_level("RPI", LoggingLevel::Error);
        let _ = mgr.log_set_level("IPAProxy", LoggingLevel::Error);

        let cameras = mgr.cameras();
        let Some(camera) = cameras.iter().find(|camera| camera.id() == camera_name) else {
            let ids: Vec<String> = cameras
                .iter()
                .map(|camera| camera.id().to_string())
                .collect();
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("libcamera camera {camera_name:?} not found, available: {ids:?}"),
            ));
        };

        let mut active = camera.acquire()?;
        let sensor_sizes = collect_raw_sensor_sizes(&mut active)?;
        if sensor_sizes.is_empty() {
            debug!(
                "No Raw sensor sizes for {camera_name:?}; falling back to VideoRecording size list"
            );
        } else {
            debug!(
                "Restricting ISP modes for {camera_name:?} to {} Raw sensor size(s)",
                sensor_sizes.len()
            );
        }

        let mut sizes_by_encode: HashMap<VideoEncodeType, HashSet<Size>> = HashMap::new();

        // ISP-processed streamables only (NV12 / RGB / YUYV). Skip Raw/Bayer.
        // Prefer discrete Raw WxH so the UI does not offer ISP-scalable fakes.
        collect_isp_formats(
            &mut active,
            if sensor_sizes.is_empty() {
                None
            } else {
                Some(&sensor_sizes)
            },
            &mut sizes_by_encode,
        )?;

        let mut formats = sizes_by_encode
            .into_iter()
            .filter(|(encode, _)| is_isp_streamable(encode))
            .map(|(encode, sizes)| {
                let mut sizes = sizes.into_iter().collect::<Vec<_>>();
                sizes.sort();
                sizes.reverse();
                Format { encode, sizes }
            })
            .collect::<Vec<_>>();
        formats.sort_by(|left, right| left.encode.cmp(&right.encode));
        Ok(formats)
    }

    fn is_isp_streamable(encode: &VideoEncodeType) -> bool {
        matches!(
            encode,
            VideoEncodeType::Nv12 | VideoEncodeType::Rgb | VideoEncodeType::Yuyv
        )
    }

    /// Discrete (width, height) from `StreamRole::Raw` — real sensor modes.
    fn collect_raw_sensor_sizes(
        camera: &mut libcamera::camera::ActiveCamera<'_>,
    ) -> io::Result<HashSet<(u32, u32)>> {
        let Some(config) = camera.generate_configuration(&[StreamRole::Raw]) else {
            debug!("No libcamera configuration for role Raw");
            return Ok(HashSet::new());
        };
        let Some(stream0) = config.get(0) else {
            return Ok(HashSet::new());
        };

        let mut sizes = HashSet::new();
        for pixel_format in stream0.formats().pixel_formats().into_iter() {
            for size in stream0.formats().sizes(pixel_format) {
                sizes.insert((size.width, size.height));
            }
        }
        Ok(sizes)
    }

    fn collect_isp_formats(
        camera: &mut libcamera::camera::ActiveCamera<'_>,
        sensor_sizes: Option<&HashSet<(u32, u32)>>,
        sizes_by_encode: &mut HashMap<VideoEncodeType, HashSet<Size>>,
    ) -> io::Result<()> {
        let role = StreamRole::VideoRecording;
        let Some(mut config) = camera.generate_configuration(&[role]) else {
            debug!("No libcamera configuration for role {role:?}");
            return Ok(());
        };
        let Some(stream0) = config.get(0) else {
            return Ok(());
        };

        let pixel_formats: Vec<_> = stream0
            .formats()
            .pixel_formats()
            .into_iter()
            .filter(|pixel_format| {
                let encode =
                    VideoEncodeType::from_str(&format!("{pixel_format:?}")).expect("irrefutable");
                is_isp_streamable(&encode)
            })
            .collect();

        let mode_list: Vec<(libcamera::pixel_format::PixelFormat, LcSize)> = match sensor_sizes {
            Some(allowed) => {
                let mut sizes: Vec<LcSize> = allowed
                    .iter()
                    .map(|&(width, height)| LcSize::new(width, height))
                    .collect();
                sizes.sort_by(|a, b| (b.width, b.height).cmp(&(a.width, a.height)));
                pixel_formats
                    .iter()
                    .flat_map(|pixel_format| {
                        sizes.iter().copied().map(|size| (*pixel_format, size))
                    })
                    .collect()
            }
            None => pixel_formats
                .iter()
                .flat_map(|pixel_format| {
                    stream0
                        .formats()
                        .sizes(*pixel_format)
                        .into_iter()
                        .map(|size| (*pixel_format, size))
                })
                .collect(),
        };

        for (pixel_format, size) in mode_list {
            let encode_name = format!("{pixel_format:?}");
            let encode = VideoEncodeType::from_str(&encode_name).expect("irrefutable");

            if let Some(mut stream0) = config.get_mut(0) {
                stream0.set_pixel_format(pixel_format);
                stream0.set_size(size);
            }

            // Do not override SensorConfiguration for VideoRecording: inventing a
            // bit-depth from names like NV12/BGR888 makes configure() fail and
            // drops all ISP formats from the list.
            config.validate();

            if let Err(error) = camera.configure(&mut config) {
                debug!(
                    "Skipping libcamera mode {encode_name} {}x{} ({role:?}): {error}",
                    size.width, size.height
                );
                if let Some(fresh) = camera.generate_configuration(&[role]) {
                    config = fresh;
                }
                continue;
            }

            let intervals = frame_intervals(camera);
            let entry = Size {
                width: size.width,
                height: size.height,
                intervals,
            };
            sizes_by_encode.entry(encode).or_default().insert(entry);

            if let Some(fresh) = camera.generate_configuration(&[role]) {
                config = fresh;
            }
        }

        Ok(())
    }

    /// Discrete FPS choices within `FrameDurationLimits` (shortest → max FPS).
    ///
    /// Libcamera exposes a continuous duration range; the UI needs a short list,
    /// so we offer common rates that fit plus the mode's exact max.
    fn frame_intervals(camera: &libcamera::camera::ActiveCamera<'_>) -> Vec<FrameInterval> {
        const CANDIDATE_FPS: &[f64] = &[
            120.0, 90.0, 60.0, 50.0, 30.0, 25.0, 24.0, 20.0, 15.0, 10.0, 5.0, 2.0, 1.0,
        ];

        let (min_duration_us, max_duration_us) = match frame_duration_limits_us(camera) {
            Some(limits) => limits,
            None => {
                return vec![FrameInterval {
                    numerator: 1,
                    denominator: 30,
                }];
            }
        };

        let max_fps = 1_000_000.0 / min_duration_us as f64;
        let min_fps = 1_000_000.0 / max_duration_us as f64;

        let mut intervals = Vec::new();
        intervals.push(fps_to_interval(max_fps));
        for &fps in CANDIDATE_FPS {
            if fps <= max_fps + 0.05 && fps >= min_fps - 0.05 {
                intervals.push(fps_to_interval(fps));
            }
        }
        intervals.sort();
        intervals.dedup();
        intervals.reverse();
        if intervals.is_empty() {
            intervals.push(fps_to_interval(max_fps));
        }
        intervals
    }

    fn frame_duration_limits_us(
        camera: &libcamera::camera::ActiveCamera<'_>,
    ) -> Option<(i64, i64)> {
        let info = match camera
            .controls()
            .find(ControlId::FrameDurationLimits as u32)
        {
            Ok(info) => info,
            Err(ControlError::NotFound(_)) => return None,
            Err(error) => {
                debug!("FrameDurationLimits lookup failed: {error}");
                return None;
            }
        };

        let min_duration_us = match FrameDurationLimits::try_from(info.min()) {
            Ok(limits) => limits.0[0],
            Err(error) => match i64::try_from(info.min()) {
                Ok(value) => value,
                Err(_) => {
                    debug!("FrameDurationLimits min decode failed: {error}");
                    return None;
                }
            },
        };

        let max_duration_us = match FrameDurationLimits::try_from(info.max()) {
            Ok(limits) => limits.0[1].max(limits.0[0]),
            Err(_) => match i64::try_from(info.max()) {
                Ok(value) => value,
                Err(error) => {
                    debug!("FrameDurationLimits max decode failed: {error}");
                    // Fall back to a 1 FPS floor when max is unavailable.
                    1_000_000
                }
            },
        };

        if min_duration_us <= 0 || max_duration_us < min_duration_us {
            return None;
        }
        Some((min_duration_us, max_duration_us))
    }

    fn fps_to_interval(fps: f64) -> FrameInterval {
        if !fps.is_finite() || fps <= 0.0 {
            return FrameInterval {
                numerator: 1,
                denominator: 30,
            };
        }
        let rounded = fps.round();
        if (fps - rounded).abs() < 0.05 {
            return FrameInterval {
                numerator: 1,
                denominator: rounded.max(1.0) as u32,
            };
        }
        FrameInterval {
            numerator: 100,
            denominator: (fps * 100.0).round().max(1.0) as u32,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fps_interval_rounds_near_integers() {
            assert_eq!(
                fps_to_interval(30.0),
                FrameInterval {
                    numerator: 1,
                    denominator: 30
                }
            );
            assert_eq!(
                fps_to_interval(21.19),
                FrameInterval {
                    numerator: 100,
                    denominator: 2119
                }
            );
        }
    }
}
