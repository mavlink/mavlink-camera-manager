//! Camera controls for libcamera sources via GStreamer `libcamerasrc` properties.
//!
//! MCM's control API is V4L-shaped (`i64` values). Float libcamera properties are
//! exposed as milli-units (`value * 1000`, `cpp_type = "int64"`) so they fit that
//! API without changing clients.
//!
//! When a stream is running, get/set targets the live `libcamerasrc` named `source`.
//! Otherwise values are kept in a pending map and applied when the pipeline starts.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Mutex, OnceLock},
};

use glib::prelude::*;
use tracing::*;

use crate::{
    controls::types::{
        Control, ControlBool, ControlMenu, ControlOption, ControlSlider, ControlState, ControlType,
    },
    stream::manager::{LiveSourceLookup, try_live_source_element},
};

const FLOAT_SCALE: f64 = 1000.0;
/// MAVLink `param_id` decimal encoding only round-trips ≤8 digits (see `mavlink::utils`).
const CONTROL_ID_SPACE: u64 = 100_000_000;

static PENDING: OnceLock<Mutex<HashMap<String, BTreeMap<String, i64>>>> = OnceLock::new();

struct SliderControlSpec<'a> {
    name: &'a str,
    id: u64,
    state: ControlState,
    min: i64,
    max: i64,
    default: i64,
    cpp_type: &'a str,
    is_live: bool,
}

/// Stable control id from a `libcamerasrc` property name.
///
/// IDs are capped below [`CONTROL_ID_SPACE`] so they MAVLink-round-trip through
/// decimal `param_id` encoding (≤8 decimal digits).
#[instrument(level = "debug")]
pub fn control_id_for_property(name: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash % CONTROL_ID_SPACE
}

/// Apply any pending control values to a freshly configured `libcamerasrc`.
///
/// Mode-like properties (`*-mode`, `ae-enable`, `awb-enable`, `af-mode`) are
/// applied before the rest so gated value properties take effect.
#[instrument(level = "debug", skip(element))]
pub fn apply_pending_to_element(camera_name: &str, element: &gst::Element) {
    let Ok(pending) = pending_controls().lock() else {
        warn!("libcamera pending controls mutex poisoned; skipping apply for {camera_name:?}");
        return;
    };
    let Some(values) = pending.get(camera_name).cloned() else {
        return;
    };
    drop(pending);

    let mode_like: Vec<String> = values
        .keys()
        .filter(|name| is_mode_like_property(name))
        .cloned()
        .collect();
    let others: Vec<String> = values
        .keys()
        .filter(|name| !is_mode_like_property(name))
        .cloned()
        .collect();

    for property in mode_like.iter().chain(others.iter()) {
        let value = values[property];
        if let Err(error) = set_property_from_api(element, property, value) {
            warn!(
                "Failed applying pending libcamera control {property:?}={value} on {camera_name:?}: {error}"
            );
        } else {
            debug!("Applied pending libcamera control {property:?}={value} on {camera_name:?}");
        }
    }
}

/// List controls for `camera_name`.
///
/// Float properties are exposed as milli-units in the `i64` slider API. On failure
/// to create/inspect `libcamerasrc`, returns an empty list (and logs a warning).
#[instrument(level = "debug")]
pub fn list_controls(camera_name: &str) -> Vec<Control> {
    let (element, is_live) = match control_element(camera_name) {
        Ok(pair) => pair,
        Err(error) => {
            warn!("Failed listing libcamera controls for {camera_name:?}: {error}");
            return vec![];
        }
    };
    list_controls_on_element(camera_name, &element, is_live)
}

/// Read the current API value for `control_id`.
///
/// While streaming, reads the live element. While idle, prefers the pending cache,
/// then the property default. Float properties use milli-units.
#[instrument(level = "debug")]
pub fn control_value_by_id(camera_name: &str, control_id: u64) -> std::io::Result<i64> {
    let (element, is_live) = control_element(camera_name)?;
    let controls = list_controls_on_element(camera_name, &element, is_live);
    let Some(control) = controls
        .into_iter()
        .find(|control| control.id == control_id)
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Control ID {control_id} was not found for libcamera device {camera_name:?}"),
        ));
    };

    Ok(property_current_api_value(
        &element,
        camera_name,
        &control.name,
        control_default(&control.configuration),
        is_live,
    ))
}

/// Set a `libcamerasrc` property by name.
///
/// - Live stream: writes the element, then caches pending for restart.
/// - Not streaming: caches pending only (applied on pipeline start).
/// - Manager lock busy: returns [`std::io::ErrorKind::WouldBlock`] without caching.
#[instrument(level = "debug")]
pub fn set_control_by_name(camera_name: &str, property: &str, value: i64) -> std::io::Result<()> {
    match try_live_source_element(camera_name) {
        LiveSourceLookup::Found(element) => {
            set_property_from_api(&element, property, value)?;
            store_pending(camera_name, property, value);
            Ok(())
        }
        LiveSourceLookup::NotStreaming => {
            store_pending(camera_name, property, value);
            debug!(
                "Cached libcamera control {property}={value} for {camera_name:?} until stream starts"
            );
            Ok(())
        }
        LiveSourceLookup::Busy => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("Stream manager busy while setting libcamera control on {camera_name:?}"),
        )),
    }
}

/// Find a single control by id, or `None` if missing / enumeration failed.
#[instrument(level = "debug")]
pub fn find_control(camera_name: &str, control_id: u64) -> Option<Control> {
    list_controls(camera_name)
        .into_iter()
        .find(|control| control.id == control_id)
}

fn pending_controls() -> &'static Mutex<HashMap<String, BTreeMap<String, i64>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_control_id(name: &str, used: &mut BTreeSet<u64>) -> u64 {
    let base = control_id_for_property(name);
    let mut id = base;
    while used.contains(&id) {
        id = (id + 1) % CONTROL_ID_SPACE;
        if id == base {
            break;
        }
    }
    used.insert(id);
    id
}

fn is_excluded_property(name: &str) -> bool {
    // Remaining name denylist for RW scalar props that are not camera controls.
    matches!(
        name,
        "name" | "parent" | "parent-class" | "camera-name" | "client-name"
    )
}

fn is_mode_like_property(name: &str) -> bool {
    name.ends_with("-mode") || matches!(name, "ae-enable" | "awb-enable" | "af-mode")
}

// ponytail: GObject float ParamSpecs often advertise ±FLT_MAX, so UI/validation
// ranges are hardcoded heuristics. Ceiling: wrong slider limits / silent clamp
// vs real sensor. Upgrade: read libcamera ControlInfo (min/max) per camera.
fn float_bounds_milli(name: &str) -> (i64, i64) {
    match name {
        "brightness" => (-1000, 1000),
        "contrast" | "saturation" | "sharpness" | "gamma" => (0, 10_000),
        "analogue-gain" | "digital-gain" => (0, 100_000),
        "exposure-value" => (-8_000, 8_000),
        "lens-position" => (0, 32_000),
        _ => (-100_000, 100_000),
    }
}

fn float_to_api(value: f64) -> i64 {
    (value * FLOAT_SCALE).round() as i64
}

fn float_from_api(value: i64) -> f64 {
    value as f64 / FLOAT_SCALE
}

fn store_pending(camera_name: &str, property: &str, value: i64) {
    let Ok(mut pending) = pending_controls().lock() else {
        warn!("libcamera pending controls mutex poisoned; dropping store for {camera_name:?}");
        return;
    };
    pending
        .entry(camera_name.to_string())
        .or_default()
        .insert(property.to_string(), value);
}

fn pending_value(camera_name: &str, property: &str) -> Option<i64> {
    let Ok(pending) = pending_controls().lock() else {
        warn!("libcamera pending controls mutex poisoned; ignoring pending for {camera_name:?}");
        return None;
    };
    pending.get(camera_name)?.get(property).copied()
}

fn probe_element() -> std::io::Result<gst::Element> {
    gst::ElementFactory::make("libcamerasrc")
        .build()
        .map_err(|error| {
            std::io::Error::other(format!("Failed to create libcamerasrc element: {error}"))
        })
}

fn control_element(camera_name: &str) -> std::io::Result<(gst::Element, bool)> {
    match try_live_source_element(camera_name) {
        LiveSourceLookup::Found(element) => Ok((element, true)),
        LiveSourceLookup::NotStreaming | LiveSourceLookup::Busy => {
            let element = probe_element()?;
            if element.has_property("camera-name") {
                element.set_property("camera-name", camera_name);
            }
            Ok((element, false))
        }
    }
}

fn control_default(configuration: &ControlType) -> i64 {
    match configuration {
        ControlType::Bool(control) => control.default,
        ControlType::Slider(control) => control.default,
        ControlType::Menu(control) => control.default,
    }
}

fn property_current_api_value(
    element: &gst::Element,
    camera_name: &str,
    property: &str,
    default: i64,
    is_live: bool,
) -> i64 {
    if is_live {
        if let Some(value) = read_property_as_api(element, property) {
            return value;
        }
        return default;
    }

    pending_value(camera_name, property).unwrap_or(default)
}

fn read_property_as_api(element: &gst::Element, property: &str) -> Option<i64> {
    let param_spec = element.find_property(property)?;
    let value = element.property_value(property);

    if glib::EnumClass::with_type(param_spec.value_type()).is_some() {
        return Some(i64::from(value.get::<i32>().ok()?));
    }

    let value_type = param_spec.value_type();
    if value_type == bool::static_type() {
        return Some(i64::from(value.get::<bool>().ok()?));
    }
    if value_type == i32::static_type() {
        return Some(i64::from(value.get::<i32>().ok()?));
    }
    if value_type == u32::static_type() {
        return Some(i64::from(value.get::<u32>().ok()?));
    }
    if value_type == i64::static_type() {
        return value.get::<i64>().ok();
    }
    if value_type == u64::static_type() {
        return value
            .get::<u64>()
            .ok()
            .map(|unsigned_value| unsigned_value as i64);
    }
    if value_type == f32::static_type() {
        return Some(float_to_api(f64::from(value.get::<f32>().ok()?)));
    }
    if value_type == f64::static_type() {
        return Some(float_to_api(value.get::<f64>().ok()?));
    }

    None
}

fn set_property_from_api(
    element: &gst::Element,
    property: &str,
    value: i64,
) -> std::io::Result<()> {
    let Some(param_spec) = element.find_property(property) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Property {property:?} not found on libcamerasrc"),
        ));
    };

    if let Some(enum_class) = glib::EnumClass::with_type(param_spec.value_type()) {
        let enum_int = i32::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Enum value {value} does not fit in i32: {error}"),
            )
        })?;
        let Some(enum_value) = enum_class.to_value(enum_int) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Enum value {value} is not valid for property {property:?}"),
            ));
        };
        element.set_property_from_value(property, &enum_value);
        return Ok(());
    }

    let value_type = param_spec.value_type();
    if value_type == bool::static_type() {
        element.set_property(property, value != 0);
        return Ok(());
    }
    if value_type == i32::static_type() {
        let narrowed = i32::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Value {value} does not fit in i32: {error}"),
            )
        })?;
        element.set_property(property, narrowed);
        return Ok(());
    }
    if value_type == u32::static_type() {
        let narrowed = u32::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Value {value} does not fit in u32: {error}"),
            )
        })?;
        element.set_property(property, narrowed);
        return Ok(());
    }
    if value_type == i64::static_type() {
        element.set_property(property, value);
        return Ok(());
    }
    if value_type == u64::static_type() {
        let narrowed = u64::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Value {value} does not fit in u64: {error}"),
            )
        })?;
        element.set_property(property, narrowed);
        return Ok(());
    }
    if value_type == f32::static_type() {
        element.set_property(property, float_from_api(value) as f32);
        return Ok(());
    }
    if value_type == f64::static_type() {
        element.set_property(property, float_from_api(value));
        return Ok(());
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "Unsupported libcamerasrc property type for {property:?}: {:?}",
            param_spec.value_type()
        ),
    ))
}

fn list_controls_on_element(
    camera_name: &str,
    element: &gst::Element,
    is_live: bool,
) -> Vec<Control> {
    let mut used_ids = BTreeSet::new();
    let mut controls = element
        .list_properties()
        .iter()
        .filter_map(|param_spec| {
            control_from_pspec(element, camera_name, param_spec, is_live, &mut used_ids)
        })
        .collect::<Vec<_>>();
    controls.sort_by(|left, right| left.name.cmp(&right.name));
    controls
}

fn control_from_pspec(
    element: &gst::Element,
    camera_name: &str,
    param_spec: &glib::ParamSpec,
    is_live: bool,
    used_ids: &mut BTreeSet<u64>,
) -> Option<Control> {
    let name = param_spec.name();
    if is_excluded_property(name) {
        return None;
    }
    if !param_spec.flags().contains(glib::ParamFlags::READABLE)
        || !param_spec.flags().contains(glib::ParamFlags::WRITABLE)
        || param_spec
            .flags()
            .contains(glib::ParamFlags::CONSTRUCT_ONLY)
    {
        return None;
    }

    let id = resolve_control_id(name, used_ids);
    // libcamerasrc does not expose inactive/disabled flags yet — always report active.
    let state = ControlState {
        is_disabled: false,
        is_inactive: false,
    };

    if let Some(enum_class) = glib::EnumClass::with_type(param_spec.value_type()) {
        let options = enum_class
            .values()
            .iter()
            .map(|enum_value| ControlOption {
                name: enum_value.nick().to_string(),
                value: i64::from(enum_value.value()),
            })
            .collect::<Vec<_>>();
        if options.is_empty() {
            return None;
        }
        let default = param_spec
            .downcast_ref::<glib::ParamSpecEnum>()
            .map(|param_spec_enum| i64::from(param_spec_enum.default_value_as_i32()))
            .unwrap_or(options[0].value);
        let value = property_current_api_value(element, camera_name, name, default, is_live);
        return Some(Control {
            name: name.to_string(),
            cpp_type: "int32".to_string(),
            id,
            state,
            configuration: ControlType::Menu(ControlMenu {
                default,
                value,
                options,
            }),
        });
    }

    let value_type = param_spec.value_type();
    if value_type == bool::static_type() {
        let default = param_spec
            .downcast_ref::<glib::ParamSpecBoolean>()
            .map(|param_spec_bool| i64::from(param_spec_bool.default_value()))
            .unwrap_or(0);
        let value = property_current_api_value(element, camera_name, name, default, is_live);
        return Some(Control {
            name: name.to_string(),
            cpp_type: "bool".to_string(),
            id,
            state,
            configuration: ControlType::Bool(ControlBool { default, value }),
        });
    }

    if value_type == i32::static_type() {
        let (min, max, default) = param_spec
            .downcast_ref::<glib::ParamSpecInt>()
            .map(|param_spec_int| {
                (
                    i64::from(param_spec_int.minimum()),
                    i64::from(param_spec_int.maximum()),
                    i64::from(param_spec_int.default_value()),
                )
            })
            .unwrap_or((i64::from(i32::MIN), i64::from(i32::MAX), 0));
        return Some(slider_control(
            element,
            camera_name,
            SliderControlSpec {
                name,
                id,
                state,
                min,
                max,
                default,
                cpp_type: "int64",
                is_live,
            },
        ));
    }

    if value_type == u32::static_type() {
        let (min, max, default) = param_spec
            .downcast_ref::<glib::ParamSpecUInt>()
            .map(|param_spec_uint| {
                (
                    i64::from(param_spec_uint.minimum()),
                    i64::from(param_spec_uint.maximum()),
                    i64::from(param_spec_uint.default_value()),
                )
            })
            .unwrap_or((0, i64::from(u32::MAX), 0));
        return Some(slider_control(
            element,
            camera_name,
            SliderControlSpec {
                name,
                id,
                state,
                min,
                max,
                default,
                cpp_type: "int64",
                is_live,
            },
        ));
    }

    if value_type == i64::static_type() {
        let (min, max, default) = param_spec
            .downcast_ref::<glib::ParamSpecInt64>()
            .map(|param_spec_int64| {
                (
                    param_spec_int64.minimum(),
                    param_spec_int64.maximum(),
                    param_spec_int64.default_value(),
                )
            })
            .unwrap_or((i64::MIN, i64::MAX, 0));
        return Some(slider_control(
            element,
            camera_name,
            SliderControlSpec {
                name,
                id,
                state,
                min,
                max,
                default,
                cpp_type: "int64",
                is_live,
            },
        ));
    }

    if value_type == f32::static_type() || value_type == f64::static_type() {
        let (pspec_min, pspec_max, pspec_default) = if let Some(param_spec_float) =
            param_spec.downcast_ref::<glib::ParamSpecFloat>()
        {
            (
                f64::from(param_spec_float.minimum()),
                f64::from(param_spec_float.maximum()),
                f64::from(param_spec_float.default_value()),
            )
        } else if let Some(param_spec_double) = param_spec.downcast_ref::<glib::ParamSpecDouble>() {
            (
                param_spec_double.minimum(),
                param_spec_double.maximum(),
                param_spec_double.default_value(),
            )
        } else {
            (f64::NEG_INFINITY, f64::INFINITY, 0.0)
        };

        let (min, max) = if pspec_min.is_finite()
            && pspec_max.is_finite()
            && pspec_max.abs() < 1_000_000.0
            && pspec_min.abs() < 1_000_000.0
        {
            (float_to_api(pspec_min), float_to_api(pspec_max))
        } else {
            float_bounds_milli(name)
        };
        let default = if pspec_default.is_finite() {
            float_to_api(pspec_default)
        } else {
            0
        };

        return Some(slider_control(
            element,
            camera_name,
            SliderControlSpec {
                name,
                id,
                state,
                min,
                max,
                default,
                // Milli-units; documented in module/`list_controls` docs.
                cpp_type: "int64",
                is_live,
            },
        ));
    }

    // Non-scalar (boxed/array/object/string) properties are skipped by type.
    None
}

fn slider_control(
    element: &gst::Element,
    camera_name: &str,
    spec: SliderControlSpec<'_>,
) -> Control {
    let value =
        property_current_api_value(element, camera_name, spec.name, spec.default, spec.is_live);
    Control {
        name: spec.name.to_string(),
        cpp_type: spec.cpp_type.to_string(),
        id: spec.id,
        state: spec.state,
        configuration: ControlType::Slider(ControlSlider {
            default: spec.default,
            value,
            step: 1,
            max: spec.max,
            min: spec.min,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_ids_are_stable() {
        assert_eq!(
            control_id_for_property("exposure-time"),
            control_id_for_property("exposure-time")
        );
        assert_ne!(
            control_id_for_property("exposure-time"),
            control_id_for_property("analogue-gain")
        );
        assert!(control_id_for_property("exposure-time") < CONTROL_ID_SPACE);
    }

    #[test]
    fn control_ids_mavlink_roundtrip() {
        fn mavlink_roundtrip(id: u64) -> Option<u64> {
            const N: usize = 16;
            let id_string = id.to_string();
            let bytes = id_string.as_bytes();
            let len = bytes.len().min(N);
            let mut buf = [0u8; N];
            buf[..len].copy_from_slice(&bytes[..len]);

            let mut parse_buf = [0u8; std::mem::size_of::<u64>()];
            let parse_len = parse_buf.len().min(N);
            parse_buf.copy_from_slice(&buf[..parse_len]);
            let Ok(id_string) =
                std::str::from_utf8(&parse_buf).map(|s| s.trim_end_matches(char::from(0)))
            else {
                return None;
            };
            id_string.parse().ok()
        }

        for name in [
            "exposure-time",
            "analogue-gain",
            "brightness",
            "contrast",
            "af-mode",
            "ae-enable",
            "awb-enable",
            "digital-gain",
            "gamma",
        ] {
            let id = control_id_for_property(name);
            assert!(id < CONTROL_ID_SPACE, "id for {name} exceeds MAVLink space");
            assert_eq!(
                mavlink_roundtrip(id),
                Some(id),
                "roundtrip failed for {name}"
            );
        }
    }

    #[test]
    fn float_scale_roundtrips() {
        assert_eq!(float_to_api(1.5), 1500);
        assert!((float_from_api(1500) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn pending_value_preferred_when_idle() {
        let camera = "test-camera-pending";
        store_pending(camera, "brightness", 250);
        assert_eq!(pending_value(camera, "brightness"), Some(250));
    }
}
