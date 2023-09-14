use anyhow::anyhow;
use tracing::*;

use super::mavlink_camera_component::MavlinkCameraComponent;

pub fn from_string_to_u8_array_with_size_32(src: &str) -> [u8; 32] {
    let bytes = src.as_bytes();
    let mut dst = [0u8; 32];
    let len = std::cmp::min(bytes.len(), 32);
    dst[..len].copy_from_slice(&bytes[..len]);
    dst
}

pub fn from_string_to_char_array_with_size_32(src: &str) -> [char; 32] {
    let chars: Vec<char> = src.chars().collect();
    let mut dst = ['\0'; 32];
    let len = std::cmp::min(chars.len(), 32);
    dst[..len].copy_from_slice(&chars[..len]);
    dst
}

pub fn from_string_to_vec_char_with_defined_size_and_null_terminator(
    src: &str,
    max_lenght: usize,
) -> Vec<char> {
    let mut ret = src.chars().take(max_lenght - 1).collect::<Vec<char>>();
    ret.push('\0');
    ret
}

pub fn get_stream_status_flag(
    component: &MavlinkCameraComponent,
) -> mavlink::common::VideoStreamStatusFlags {
    match component.thermal {
        true => mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_THERMAL,
        false => mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_RUNNING,
    }
}

pub fn param_value_from_control_value(control_value: i64, length: usize) -> Vec<char> {
    let mut param_value = control_value
        .to_le_bytes()
        .iter()
        .map(|&byte| byte as char)
        .collect::<Vec<char>>();
    // Workaround for https://github.com/mavlink/rust-mavlink/issues/111
    param_value.resize(length, Default::default());
    param_value
}

pub fn control_value_from_param_value(
    param_value: &[char],
    param_type: &mavlink::common::MavParamExtType,
) -> Option<i64> {
    let bytes: Vec<u8> = param_value.iter().map(|c| *c as u8).collect();
    let control_value = match param_type {
        mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_UINT8 => {
            Ok(u8::from_ne_bytes(bytes[0..1].try_into().unwrap()) as i64)
        }
        mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT32 => {
            Ok(i32::from_ne_bytes(bytes[0..4].try_into().unwrap()) as i64)
        }
        mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT64 => {
            Ok(i64::from_ne_bytes(bytes[0..8].try_into().unwrap()))
        }
        something_else => Err(anyhow!(
            "Received parameter of untreatable type: {something_else:#?}",
        )),
    };
    if let Err(error) = control_value {
        error!("Failed to parse parameter value: {error:#?}.");
        return None;
    }
    control_value.ok()
}

pub fn get_param_index_and_control_id(
    param_ext_req: &mavlink::common::PARAM_EXT_REQUEST_READ_DATA,
    controls: &[crate::video::types::Control],
) -> Option<(u16, u64)> {
    let param_index = param_ext_req.param_index;
    // Use param_index if it is !=1, otherwise, use param_id. For more information: https://mavlink.io/en/messages/common.html#PARAM_EXT_REQUEST_READ
    let (param_index, control_id) = if param_index == -1 {
        let control_id = match control_id_from_param_id(&param_ext_req.param_id) {
            Some(value) => value,
            None => return None,
        };

        match &controls.iter().position(|control| control_id == control.id) {
            Some(param_index) => (*param_index as i16, control_id),
            None => {
                error!("Failed to find control id {control_id}.");
                return None;
            }
        }
    } else {
        match &controls.get(param_index as usize) {
            Some(control) => (param_index, control.id),
            None => {
                error!("Failed to find control index {param_index}.");
                return None;
            }
        }
    };
    Some((param_index as u16, control_id))
}

pub fn param_id_from_control_id(id: u64) -> [char; 16] {
    let mut param_id: [char; 16] = Default::default();
    id.to_string()
        .chars()
        .zip(param_id.iter_mut())
        .for_each(|(a, b)| *b = a);
    param_id
}

pub fn control_id_from_param_id(param_id: &[char; 16]) -> Option<u64> {
    let control_id = param_id
        .iter()
        .collect::<String>()
        .trim_end_matches(char::from(0))
        .parse::<u64>();
    if let Err(error) = control_id {
        error!("Failed to parse control id: {error:#?}.");
        return None;
    }
    control_id.ok()
}
