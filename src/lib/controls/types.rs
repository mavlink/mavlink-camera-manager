use paperclip::actix::Apiv2Schema;
use serde::Serialize;

#[derive(Apiv2Schema, Clone, Debug, Default, Serialize)]
pub struct Control {
    pub name: String,
    pub cpp_type: String,
    pub id: u64,
    pub state: ControlState,
    pub configuration: ControlType,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub enum ControlType {
    Bool(ControlBool),
    Slider(ControlSlider),
    Menu(ControlMenu),
}

#[derive(Apiv2Schema, Clone, Debug, Default, Serialize)]
pub struct ControlState {
    pub is_disabled: bool,
    pub is_inactive: bool,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlBool {
    pub default: i64,
    pub value: i64,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlSlider {
    pub default: i64,
    pub value: i64,
    pub step: u64,
    pub max: i64,
    pub min: i64,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlMenu {
    pub default: i64,
    pub value: i64,
    pub options: Vec<ControlOption>,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct ControlOption {
    pub name: String,
    pub value: i64,
}

impl Default for ControlType {
    fn default() -> Self {
        ControlType::Bool(ControlBool {
            default: 0,
            value: 0,
        })
    }
}
