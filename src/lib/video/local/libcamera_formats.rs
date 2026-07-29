//! Native libcamera format / mode enumeration (rpicam-apps style).
//!
//! GStreamer's `libcamerasrc` device-monitor caps invent a large STANDARD_SIZES
//! grid and omit framerates. With the `libcamera-native` feature this module
//! mirrors `rpicam-hello --list-cameras`: acquire the camera, walk
//! `StreamRole::Raw` (Bayer) and `StreamRole::VideoRecording` (processed)
//! stream formats, configure each discrete size, and read
//! `FrameDurationLimits` for the max FPS.
//!
//! Bayer formats are listed for API discovery only — MCM does not stream them yet.
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
        camera::SensorConfiguration,
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
        let mut sizes_by_encode: HashMap<VideoEncodeType, HashSet<Size>> = HashMap::new();

        collect_role_formats(&mut active, StreamRole::Raw, &mut sizes_by_encode)?;
        collect_role_formats(
            &mut active,
            StreamRole::VideoRecording,
            &mut sizes_by_encode,
        )?;

        let mut formats = sizes_by_encode
            .into_iter()
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

    fn collect_role_formats(
        camera: &mut libcamera::camera::ActiveCamera<'_>,
        role: StreamRole,
        sizes_by_encode: &mut HashMap<VideoEncodeType, HashSet<Size>>,
    ) -> io::Result<()> {
        let Some(mut config) = camera.generate_configuration(&[role]) else {
            debug!("No libcamera configuration for role {role:?}");
            return Ok(());
        };
        let Some(stream0) = config.get(0) else {
            return Ok(());
        };

        let pixel_formats: Vec<_> = stream0.formats().pixel_formats().into_iter().collect();
        let mode_list: Vec<(libcamera::pixel_format::PixelFormat, LcSize)> = pixel_formats
            .iter()
            .flat_map(|pixel_format| {
                stream0
                    .formats()
                    .sizes(*pixel_format)
                    .into_iter()
                    .map(|size| (*pixel_format, size))
            })
            .collect();

        for (pixel_format, size) in mode_list {
            let encode_name = format!("{pixel_format:?}");
            let encode = VideoEncodeType::from_str(&encode_name).expect("irrefutable");

            if let Some(mut stream0) = config.get_mut(0) {
                stream0.set_pixel_format(pixel_format);
                stream0.set_size(size);
            }

            let mut sensor = SensorConfiguration::new();
            sensor.set_output_size(size.width, size.height);
            sensor.set_bit_depth(bit_depth_from_format_name(&encode_name));
            config.set_sensor_configuration(sensor);
            let _ = config.validate();

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

            let interval = max_fps_interval(camera);
            let entry = Size {
                width: size.width,
                height: size.height,
                intervals: vec![interval],
            };
            sizes_by_encode.entry(encode).or_default().insert(entry);

            if let Some(fresh) = camera.generate_configuration(&[role]) {
                config = fresh;
            }
        }

        Ok(())
    }

    fn max_fps_interval(camera: &libcamera::camera::ActiveCamera<'_>) -> FrameInterval {
        match camera
            .controls()
            .find(ControlId::FrameDurationLimits as u32)
        {
            Ok(info) => match FrameDurationLimits::try_from(info.min()) {
                Ok(limits) => {
                    let min_duration_us = limits.0[0];
                    if min_duration_us > 0 {
                        return fps_to_interval(1_000_000.0 / min_duration_us as f64);
                    }
                }
                Err(error) => {
                    if let Ok(min_duration_us) = i64::try_from(info.min()) {
                        if min_duration_us > 0 {
                            return fps_to_interval(1_000_000.0 / min_duration_us as f64);
                        }
                    }
                    debug!("FrameDurationLimits min decode failed: {error}");
                }
            },
            Err(ControlError::NotFound(_)) => {}
            Err(error) => debug!("FrameDurationLimits lookup failed: {error}"),
        }

        FrameInterval {
            numerator: 1,
            denominator: 30,
        }
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

    fn bit_depth_from_format_name(name: &str) -> u32 {
        let mut depth = 0u32;
        let mut seen_digit = false;
        for ch in name.chars() {
            if ch.is_ascii_digit() {
                seen_digit = true;
                depth = depth
                    .saturating_mul(10)
                    .saturating_add(u32::from(ch.to_digit(10).unwrap_or(0)));
            } else if seen_digit {
                break;
            }
        }
        if depth == 0 { 12 } else { depth }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bit_depth_parses_rpicam_names() {
            assert_eq!(bit_depth_from_format_name("SRGGB10_CSI2P"), 10);
            assert_eq!(bit_depth_from_format_name("SRGGB8"), 8);
            assert_eq!(bit_depth_from_format_name("SRGGB16"), 16);
            assert_eq!(bit_depth_from_format_name("NV12"), 12);
        }

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
