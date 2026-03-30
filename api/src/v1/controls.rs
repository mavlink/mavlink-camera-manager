use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Default, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct Control {
    pub name: String,
    pub cpp_type: String,
    #[ts(type = "number")]
    pub id: u64,
    pub state: ControlState,
    pub configuration: ControlType,
}

#[derive(Debug, Clone, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub enum ControlType {
    Bool(ControlBool),
    Slider(ControlSlider),
    Menu(ControlMenu),
}

#[derive(Debug, Clone, Default, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ControlState {
    pub is_disabled: bool,
    pub is_inactive: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ControlBool {
    #[ts(type = "number")]
    pub default: i64,
    #[ts(type = "number")]
    pub value: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ControlSlider {
    #[ts(type = "number")]
    pub default: i64,
    #[ts(type = "number")]
    pub value: i64,
    #[ts(type = "number")]
    pub step: u64,
    #[ts(type = "number")]
    pub max: i64,
    #[ts(type = "number")]
    pub min: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ControlMenu {
    #[ts(type = "number")]
    pub default: i64,
    #[ts(type = "number")]
    pub value: i64,
    pub options: Vec<ControlOption>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[cfg_attr(feature = "paperclip", derive(paperclip::actix::Apiv2Schema))]
pub struct ControlOption {
    pub name: String,
    #[ts(type = "number")]
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
