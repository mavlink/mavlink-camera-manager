use std::panic::{AssertUnwindSafe, catch_unwind};

use glib::prelude::*;

pub const FLOAT_SCALE: f64 = 1000.0;

pub fn float_to_api(value: f64) -> i64 {
    let scaled = value * FLOAT_SCALE;
    if !scaled.is_finite() {
        return 0;
    }
    scaled.clamp(i64::MIN as f64, i64::MAX as f64) as i64
}

pub fn float_from_api(value: i64) -> f64 {
    value as f64 / FLOAT_SCALE
}

pub fn enum_value_by_nick(element: &gst::Element, property: &str, nick: &str) -> Option<i64> {
    let param_spec = element.find_property(property)?;
    let enum_class = glib::EnumClass::with_type(param_spec.value_type())?;
    enum_class
        .values()
        .iter()
        .find(|enum_value| enum_value.nick().eq_ignore_ascii_case(nick))
        .map(|enum_value| i64::from(enum_value.value()))
}

pub fn flags_bits_by_nick(element: &gst::Element, property: &str, nick: &str) -> Option<i64> {
    let param_spec = element.find_property(property)?;
    let flags_class = glib::FlagsClass::with_type(param_spec.value_type())?;
    flags_class.from_nick_string(nick).ok().map(i64::from)
}

pub fn set_property_from_api(
    element: &gst::Element,
    property: &str,
    value: i64,
) -> std::io::Result<()> {
    let Some(param_spec) = element.find_property(property) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Property {property:?} not found on {element:?}"),
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
        return set_property_from_value(element, property, &enum_value);
    }

    if let Some(flags_class) = glib::FlagsClass::with_type(param_spec.value_type()) {
        let flags_bits = u32::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Flags value {value} does not fit in u32: {error}"),
            )
        })?;
        let Some(flags_value) = glib_value_from_flag_bits(&flags_class, flags_bits) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Flags value {value} is not valid for property {property:?}"),
            ));
        };
        return set_property_from_value(element, property, &flags_value);
    }

    let value_type = param_spec.value_type();
    if value_type == bool::static_type() {
        return set_property_from_value(element, property, &(value != 0).to_value());
    }
    if value_type == i32::static_type() {
        let narrowed = i32::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Value {value} does not fit in i32: {error}"),
            )
        })?;
        return set_property_from_value(element, property, &narrowed.to_value());
    }
    if value_type == u32::static_type() {
        let narrowed = u32::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Value {value} does not fit in u32: {error}"),
            )
        })?;
        return set_property_from_value(element, property, &narrowed.to_value());
    }
    if value_type == i64::static_type() {
        return set_property_from_value(element, property, &value.to_value());
    }
    if value_type == u64::static_type() {
        let narrowed = u64::try_from(value).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Value {value} does not fit in u64: {error}"),
            )
        })?;
        return set_property_from_value(element, property, &narrowed.to_value());
    }
    if value_type == f32::static_type() {
        let float_value = float_from_api(value) as f32;
        return set_property_from_value(element, property, &float_value.to_value());
    }
    if value_type == f64::static_type() {
        let float_value = float_from_api(value);
        return set_property_from_value(element, property, &float_value.to_value());
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "Unsupported property type for {property:?}: {:?}",
            param_spec.value_type()
        ),
    ))
}

fn glib_value_from_flag_bits(flags_class: &glib::FlagsClass, bits: u32) -> Option<glib::Value> {
    let mut builder = flags_class.builder();
    for flag in flags_class.values() {
        if bits & flag.value() != 0 {
            builder = builder.set(flag.value());
        }
    }
    builder.build()
}

pub fn set_property_from_value(
    element: &gst::Element,
    property: &str,
    value: &glib::Value,
) -> std::io::Result<()> {
    catch_unwind(AssertUnwindSafe(|| {
        element.set_property_from_value(property, value);
    }))
    .map_err(|_| std::io::Error::other(format!("Element panicked while setting {property:?}")))
}
