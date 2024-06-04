use anyhow::anyhow;
use tracing::*;

use super::mavlink_camera_component::MavlinkCameraComponent;

#[instrument(level = "debug")]
pub fn from_string_to_sized_u8_array_with_null_terminator<const N: usize>(src: &str) -> [u8; N] {
    let mut buf = [0u8; N];

    let bytes = src.as_bytes();
    let len = bytes.len().min(N - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
    buf[N - 1] = b'\0';

    buf
}

#[instrument(level = "debug")]
pub fn get_stream_status_flag(
    component: &MavlinkCameraComponent,
) -> mavlink::common::VideoStreamStatusFlags {
    match component.thermal {
        true => mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_THERMAL,
        false => mavlink::common::VideoStreamStatusFlags::VIDEO_STREAM_STATUS_FLAGS_RUNNING,
    }
}

#[instrument(level = "debug")]
pub fn param_value_from_control_value<const N: usize>(control_value: i64) -> [u8; N] {
    let bytes = control_value.to_le_bytes();
    let len = bytes.len().min(N);

    let mut buf = [0u8; N];
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

#[instrument(level = "debug")]
pub fn control_value_from_param_value(
    param_value: &[u8],
    param_type: &mavlink::common::MavParamExtType,
) -> Option<i64> {
    let control_value = match param_type {
        mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_UINT8 => Ok(u8::from_le_bytes(
            param_value[0..std::mem::size_of::<u8>()]
                .try_into()
                .unwrap(),
        ) as i64),
        mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT32 => Ok(i32::from_le_bytes(
            param_value[0..std::mem::size_of::<i32>()]
                .try_into()
                .unwrap(),
        ) as i64),
        mavlink::common::MavParamExtType::MAV_PARAM_EXT_TYPE_INT64 => Ok(i64::from_le_bytes(
            param_value[0..std::mem::size_of::<i64>()]
                .try_into()
                .unwrap(),
        )),
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

#[instrument(level = "debug", skip(controls))]
pub fn get_param_index_and_control_id(
    param_ext_req: &mavlink::common::PARAM_EXT_REQUEST_READ_DATA,
    controls: &[crate::video::types::Control],
) -> Option<(u16, u64)> {
    let param_index = param_ext_req.param_index;
    // Use param_index if it is !=1, otherwise, use param_id. For more information: https://mavlink.io/en/messages/common.html#PARAM_EXT_REQUEST_READ
    let (param_index, control_id) = if param_index == -1 {
        let control_id = control_id_from_param_id(&param_ext_req.param_id)?;

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

#[instrument(level = "debug")]
pub fn param_id_from_control_id<const N: usize>(id: u64) -> [u8; N] {
    let id_string = id.to_string();
    let bytes = id_string.as_bytes();

    let len = bytes.len().min(N);

    let mut buf = [0u8; N];
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

#[instrument(level = "debug")]
pub fn control_id_from_param_id<const N: usize>(param_id: &[u8; N]) -> Option<u64> {
    let mut buf = [0u8; std::mem::size_of::<u64>()];
    let len = buf.len().min(N);
    buf.copy_from_slice(&param_id[..len]);

    let Ok(id_string) = std::str::from_utf8(&buf).map(|s| s.trim_end_matches(char::from(0))) else {
        return None;
    };

    id_string.parse().ok()
}
