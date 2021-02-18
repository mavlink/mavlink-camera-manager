use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize)]
pub struct Control {
    pub name: String,
    pub cpp_type: String,
    pub v4l2_id: u32,
    pub configuration: ControlType,
}

#[derive(Debug, Serialize)]
pub enum ControlType {
    Bool(ControlBool),
    Slider(ControlSlider),
    Menu(ControlMenu),
}

impl Default for ControlType {
    fn default() -> Self {
        ControlType::Bool(ControlBool {
            default: 0,
            value: 0,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct ControlBool {
    pub default: i32,
    pub value: i64,
}

#[derive(Debug, Serialize)]
pub struct ControlSlider {
    pub default: i32,
    pub value: i64,
    pub step: i32,
    pub max: i32,
    pub min: i32,
}

#[derive(Debug, Serialize)]
pub struct ControlMenu {
    pub default: i32,
    pub value: i64,
    pub options: Vec<ControlOption>,
}

#[derive(Debug, Serialize)]
pub struct ControlOption {
    pub name: String,
    pub value: i64,
}