use glib::prelude::*;

use crate::controls::types::{
    Control, ControlBool, ControlFlags, ControlMenu, ControlOption, ControlSlider, ControlState,
    ControlType,
};
use crate::stream::gst::docs::gst_property_docs_url;

use super::apply::float_to_api;
use super::mutable::{mutable_in_playing, requires_restart};

/// Treat ParamSpec min/max as hollow when either side is this large (e.g. +/-FLT_MAX).
const HOLLOW_BOUND: f64 = 1_000_000.0;

struct SliderControlSpec<'a> {
    name: &'a str,
    element: &'a str,
    id: u64,
    state: ControlState,
    min: i64,
    max: i64,
    default: i64,
    cpp_type: &'a str,
    mutable_in_playing: bool,
    requires_restart: bool,
    nick: String,
    blurb: Option<String>,
    docs_url: Option<String>,
}

pub fn pspec_range_is_type_wide(min: i64, max: i64) -> bool {
    (min == i64::from(i32::MIN) && max == i64::from(i32::MAX))
        || (min == 0 && max == i64::from(u32::MAX))
        || (min == i64::MIN && max == i64::MAX)
}

pub fn float_pspec_range_is_usable(min: f64, max: f64) -> bool {
    min.is_finite()
        && max.is_finite()
        && max > min
        && min.abs() < HOLLOW_BOUND
        && max.abs() < HOLLOW_BOUND
}

pub fn integer_slider_limits(min: i64, max: i64, default: i64) -> Option<(i64, i64, i64)> {
    if max > min && !pspec_range_is_type_wide(min, max) {
        return Some((min, max, default));
    }
    if max > min {
        Some((min, max, default))
    } else {
        None
    }
}

pub fn float_slider_limits(min: f64, max: f64, default: f64) -> Option<(i64, i64, i64)> {
    if float_pspec_range_is_usable(min, max) {
        let default = if default.is_finite() {
            float_to_api(default)
        } else {
            0
        };
        return Some((float_to_api(min), float_to_api(max), default));
    }
    let api_min = float_to_api(min);
    let api_max = float_to_api(max);
    if api_max > api_min {
        let default = if default.is_finite() {
            float_to_api(default)
        } else {
            0
        };
        Some((api_min, api_max, default))
    } else {
        None
    }
}

fn control_docs(
    param_spec: &glib::ParamSpec,
    factory_name: &str,
) -> (String, Option<String>, Option<String>) {
    let nick = param_spec.nick();
    let nick = if nick.is_empty() {
        param_spec.name().to_string()
    } else {
        nick.to_string()
    };
    let blurb = param_spec
        .blurb()
        .map(|blurb| blurb.to_string())
        .filter(|blurb| !blurb.is_empty());
    let docs_url = gst_property_docs_url(factory_name, param_spec.name());
    (nick, blurb, docs_url)
}

pub fn control_from_pspec(
    param_spec: &glib::ParamSpec,
    element_name: &str,
    id: u64,
) -> Option<Control> {
    let name = param_spec.name();
    let (nick, blurb, docs_url) = control_docs(param_spec, element_name);
    let state = ControlState {
        is_disabled: false,
        is_inactive: false,
    };
    let mutable_in_playing = mutable_in_playing(param_spec);
    let requires_restart = requires_restart(param_spec);

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
        return Some(Control {
            name: name.to_string(),
            element: element_name.to_string(),
            cpp_type: "int32".to_string(),
            id,
            state,
            configuration: ControlType::Menu(ControlMenu {
                default,
                value: default,
                options,
            }),
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
            plugin_name: None,
        });
    }

    if let Some(flags_class) = glib::FlagsClass::with_type(param_spec.value_type()) {
        let flags = flags_class
            .values()
            .iter()
            .map(|flag_value| ControlOption {
                name: flag_value.nick().to_string(),
                value: i64::from(flag_value.value()),
            })
            .collect::<Vec<_>>();
        if flags.is_empty() {
            return None;
        }
        let default = param_spec
            .downcast_ref::<glib::ParamSpecFlags>()
            .map(|param_spec_flags| i64::from(param_spec_flags.default_value_as_u32()))
            .unwrap_or(0);
        return Some(Control {
            name: name.to_string(),
            element: element_name.to_string(),
            cpp_type: "int64".to_string(),
            id,
            state,
            configuration: ControlType::Flags(ControlFlags {
                default,
                value: default,
                flags,
            }),
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
            plugin_name: None,
        });
    }

    let value_type = param_spec.value_type();
    if value_type == bool::static_type() {
        let default = param_spec
            .downcast_ref::<glib::ParamSpecBoolean>()
            .map(|param_spec_bool| i64::from(param_spec_bool.default_value()))
            .unwrap_or(0);
        return Some(Control {
            name: name.to_string(),
            element: element_name.to_string(),
            cpp_type: "bool".to_string(),
            id,
            state,
            configuration: ControlType::Bool(ControlBool {
                default,
                value: default,
            }),
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
            plugin_name: None,
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
        let (min, max, default) = integer_slider_limits(min, max, default)?;
        return Some(slider_control(SliderControlSpec {
            name,
            element: element_name,
            id,
            state,
            min,
            max,
            default,
            cpp_type: "int64",
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
        }));
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
        let (min, max, default) = integer_slider_limits(min, max, default)?;
        return Some(slider_control(SliderControlSpec {
            name,
            element: element_name,
            id,
            state,
            min,
            max,
            default,
            cpp_type: "int64",
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
        }));
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
        let (min, max, default) = integer_slider_limits(min, max, default)?;
        return Some(slider_control(SliderControlSpec {
            name,
            element: element_name,
            id,
            state,
            min,
            max,
            default,
            cpp_type: "int64",
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
        }));
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

        let (min, max, default) = float_slider_limits(pspec_min, pspec_max, pspec_default)?;

        return Some(slider_control(SliderControlSpec {
            name,
            element: element_name,
            id,
            state,
            min,
            max,
            default,
            cpp_type: "int64",
            mutable_in_playing,
            requires_restart,
            nick: nick.clone(),
            blurb: blurb.clone(),
            docs_url: docs_url.clone(),
        }));
    }

    None
}

fn slider_control(spec: SliderControlSpec<'_>) -> Control {
    Control {
        name: spec.name.to_string(),
        element: spec.element.to_string(),
        cpp_type: spec.cpp_type.to_string(),
        id: spec.id,
        state: spec.state,
        configuration: ControlType::Slider(ControlSlider {
            default: spec.default,
            value: spec.default,
            step: 1,
            max: spec.max,
            min: spec.min,
        }),
        mutable_in_playing: spec.mutable_in_playing,
        requires_restart: spec.requires_restart,
        nick: spec.nick,
        blurb: spec.blurb,
        docs_url: spec.docs_url,
        plugin_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_scale_roundtrips() {
        assert_eq!(float_to_api(1.5), 1500);
        assert!((super::super::apply::float_from_api(1500) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn type_wide_pspec_ranges_are_hollow() {
        assert!(pspec_range_is_type_wide(
            i64::from(i32::MIN),
            i64::from(i32::MAX)
        ));
        assert!(!pspec_range_is_type_wide(1000, 10_667));
        assert!(!float_pspec_range_is_usable(
            f64::NEG_INFINITY,
            f64::INFINITY
        ));
        assert!(float_pspec_range_is_usable(-1.0, 1.0));
        assert_eq!(
            integer_slider_limits(i64::from(i32::MIN), i64::from(i32::MAX), 0),
            Some((i64::from(i32::MIN), i64::from(i32::MAX), 0))
        );
        assert_eq!(
            integer_slider_limits(1, 1_000_000, 0),
            Some((1, 1_000_000, 0))
        );
        let (min, max, _default) =
            float_slider_limits(f64::from(-f32::MAX), f64::from(f32::MAX), 0.0)
                .expect("finite after clamp");
        assert!(max > min);
        assert_eq!(
            float_slider_limits(-1.0, 1.0, 0.0),
            Some((float_to_api(-1.0), float_to_api(1.0), 0))
        );
    }

    #[test]
    fn x264enc_speed_preset_includes_nick_blurb_and_docs_url() {
        let _ = gst::init();
        let encoder = gst::ElementFactory::make("x264enc")
            .build()
            .expect("x264enc");
        let param_spec = encoder.find_property("speed-preset").expect("speed-preset");
        let control = control_from_pspec(&param_spec, "x264enc", 1).expect("control");
        assert_eq!(control.nick, "Speed/quality preset");
        assert!(
            control
                .blurb
                .as_deref()
                .is_some_and(|blurb| blurb.contains("speed/quality")),
            "blurb={:?}",
            control.blurb
        );
        assert_eq!(
            control.docs_url,
            crate::stream::gst::docs::gst_property_docs_url("x264enc", "speed-preset")
        );
    }
}
