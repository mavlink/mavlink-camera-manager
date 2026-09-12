mod apply;
mod mutable;
pub mod pspec;

use std::{
    collections::{BTreeSet, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Mutex, MutexGuard, OnceLock},
};

use glib::prelude::*;
use gst::prelude::{ElementExt, GstObjectExt};

use crate::controls::types::Control;

pub use apply::{
    FLOAT_SCALE, enum_value_by_nick, flags_bits_by_nick, float_from_api, float_to_api,
    set_property_from_api, set_property_from_value,
};

/// MAVLink `param_id` decimal encoding only round-trips <=8 digits (see `mavlink::utils`).
pub const SOURCE_CONTROL_ID_SPACE: u64 = 50_000_000;
pub const PIPELINE_CONTROL_ID_OFFSET: u64 = 50_000_000;

static SCHEMA_CACHE: OnceLock<Mutex<HashMap<String, Vec<Control>>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlFilter {
    Libcamera,
    Encoder,
}

pub fn hash_name(name: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn source_control_id_for_property(name: &str) -> u64 {
    hash_name(name) % SOURCE_CONTROL_ID_SPACE
}

pub fn pipeline_control_id_for_element_property(element: &str, property: &str) -> u64 {
    PIPELINE_CONTROL_ID_OFFSET
        + hash_name(&format!("{element}:{property}")) % SOURCE_CONTROL_ID_SPACE
}

pub fn resolve_source_control_id(name: &str, used: &mut BTreeSet<u64>) -> u64 {
    resolve_control_id_in_space(
        source_control_id_for_property(name),
        0,
        SOURCE_CONTROL_ID_SPACE,
        used,
    )
}

pub fn resolve_pipeline_control_id(element: &str, property: &str, used: &mut BTreeSet<u64>) -> u64 {
    resolve_control_id_in_space(
        pipeline_control_id_for_element_property(element, property),
        PIPELINE_CONTROL_ID_OFFSET,
        SOURCE_CONTROL_ID_SPACE,
        used,
    )
}

pub fn resolve_control_id_in_space(
    base: u64,
    space_start: u64,
    space_size: u64,
    used: &mut BTreeSet<u64>,
) -> u64 {
    let space_end = space_start + space_size;
    let mut id = base;
    while used.contains(&id) {
        id += 1;
        if id >= space_end {
            id = space_start;
        }
        if id == base {
            break;
        }
    }
    used.insert(id);
    id
}

pub fn is_libcamera_controllable(param_spec: &glib::ParamSpec) -> bool {
    param_spec.flags().contains(gst::PARAM_FLAG_CONTROLLABLE)
        && param_spec.flags().contains(glib::ParamFlags::READABLE)
        && param_spec.flags().contains(glib::ParamFlags::WRITABLE)
        && !param_spec
            .flags()
            .contains(glib::ParamFlags::CONSTRUCT_ONLY)
}

pub fn is_encoder_controllable(param_spec: &glib::ParamSpec, element: &gst::Element) -> bool {
    param_spec.flags().contains(glib::ParamFlags::READABLE)
        && param_spec.flags().contains(glib::ParamFlags::WRITABLE)
        && !param_spec
            .flags()
            .contains(glib::ParamFlags::CONSTRUCT_ONLY)
        && param_spec.owner_type() == element.type_()
}

fn factory_schema_is_cacheable(factory_name: &str) -> bool {
    // V4L2 encoder elements only expose their full property list after opening a device.
    if factory_name.starts_with("v4l2") && factory_name.contains("enc") {
        return false;
    }
    true
}

fn schema_cache() -> &'static Mutex<HashMap<String, Vec<Control>>> {
    SCHEMA_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn recoverable_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn cached_controls_for_factory(factory_name: &str) -> Option<Vec<Control>> {
    recoverable_lock(schema_cache()).get(factory_name).cloned()
}

pub fn store_cached_controls_for_factory(factory_name: &str, controls: Vec<Control>) {
    recoverable_lock(schema_cache()).insert(factory_name.to_string(), controls);
}

pub fn list_controls_on_element(
    element: &gst::Element,
    control_element_name: &str,
    documentation_factory_name: &str,
    filter: ControlFilter,
) -> Vec<Control> {
    list_controls_on_element_with_names(
        element,
        control_element_name,
        documentation_factory_name,
        filter,
    )
}

pub fn list_controls_on_element_with_names(
    element: &gst::Element,
    control_element_name: &str,
    documentation_factory_name: &str,
    filter: ControlFilter,
) -> Vec<Control> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut used_ids = BTreeSet::new();
        let mut controls = element
            .list_properties()
            .iter()
            .filter(|param_spec| match filter {
                ControlFilter::Libcamera => is_libcamera_controllable(param_spec),
                ControlFilter::Encoder => is_encoder_controllable(param_spec, element),
            })
            .filter_map(|param_spec| {
                let id = match filter {
                    ControlFilter::Libcamera => {
                        resolve_source_control_id(param_spec.name(), &mut used_ids)
                    }
                    ControlFilter::Encoder => resolve_pipeline_control_id(
                        control_element_name,
                        param_spec.name(),
                        &mut used_ids,
                    ),
                };
                let mut control =
                    pspec::control_from_pspec(param_spec, documentation_factory_name, id)?;
                control.element = control_element_name.to_string();
                Some(control)
            })
            .collect::<Vec<_>>();
        controls.sort_by(|left, right| left.name.cmp(&right.name));
        overlay_plugin_name(&mut controls, documentation_factory_name);
        controls
    }))
    .unwrap_or_default()
}

pub fn plugin_name_for_factory(factory_name: &str) -> Option<String> {
    use gst::prelude::*;
    gst::ElementFactory::find(factory_name).and_then(|factory| {
        factory
            .plugin()
            .map(|plugin| plugin.plugin_name().to_string())
    })
}

pub fn overlay_plugin_name(controls: &mut [Control], factory_name: &str) {
    let plugin_name = plugin_name_for_factory(factory_name);
    for control in controls {
        control.plugin_name = plugin_name.clone();
    }
}

pub fn list_controls_on_live_element(element: &gst::Element) -> Vec<Control> {
    let control_element_name = element.name();
    let documentation_factory_name = element
        .factory()
        .map(|factory| factory.name())
        .unwrap_or_else(|| control_element_name.clone());
    list_controls_on_element_with_names(
        element,
        control_element_name.as_ref(),
        documentation_factory_name.as_ref(),
        ControlFilter::Encoder,
    )
}

fn list_codec_controls_from_factory(factory_name: &str) -> Vec<Control> {
    if factory_schema_is_cacheable(factory_name) {
        if let Some(cached) = cached_controls_for_factory(factory_name) {
            let mut controls = cached;
            overlay_plugin_name(&mut controls, factory_name);
            return controls;
        }
    }

    let element = match gst::ElementFactory::make(factory_name).build() {
        Ok(element) => element,
        Err(_) => return vec![],
    };
    let controls = list_controls_on_element_with_names(
        &element,
        factory_name,
        factory_name,
        ControlFilter::Encoder,
    );
    if factory_schema_is_cacheable(factory_name) && !controls.is_empty() {
        store_cached_controls_for_factory(factory_name, controls.clone());
    }
    controls
}

pub fn list_encoder_controls(factory_name: &str) -> Vec<Control> {
    list_codec_controls_from_factory(factory_name)
}

pub fn list_decoder_controls(factory_name: &str) -> Vec<Control> {
    list_codec_controls_from_factory(factory_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_ids_are_stable() {
        assert_eq!(
            source_control_id_for_property("exposure-time"),
            source_control_id_for_property("exposure-time")
        );
        assert_ne!(
            source_control_id_for_property("exposure-time"),
            source_control_id_for_property("analogue-gain")
        );
        assert!(source_control_id_for_property("exposure-time") < SOURCE_CONTROL_ID_SPACE);
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
            let id = source_control_id_for_property(name);
            assert!(
                id < SOURCE_CONTROL_ID_SPACE,
                "id for {name} exceeds source control space"
            );
            assert_eq!(
                mavlink_roundtrip(id),
                Some(id),
                "roundtrip failed for {name}"
            );
        }
    }

    #[test]
    fn pipeline_control_ids_are_in_pipeline_namespace() {
        let id = pipeline_control_id_for_element_property("x264enc", "bitrate");
        assert!(id >= PIPELINE_CONTROL_ID_OFFSET);
        assert!(id < PIPELINE_CONTROL_ID_OFFSET + SOURCE_CONTROL_ID_SPACE);
        assert_ne!(
            id,
            source_control_id_for_property("bitrate"),
            "pipeline and source namespaces must not overlap"
        );
    }

    #[test]
    fn controllable_flag_is_gstreamer_user_bit() {
        assert_eq!(gst::PARAM_FLAG_CONTROLLABLE, glib::ParamFlags::USER_1);
    }
}
