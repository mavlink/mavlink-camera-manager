use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Control {
    pub name: String,
    pub cpp_type: String,
    pub id: u64,
    pub configuration: ControlType,
}

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
pub struct ControlBool {
    pub default: i32,
    pub value: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlSlider {
    pub default: i32,
    pub value: i64,
    pub step: i32,
    pub max: i32,
    pub min: i32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlMenu {
    pub default: i32,
    pub value: i64,
    pub options: Vec<ControlOption>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlOption {
    pub name: String,
    pub value: i64,
}
