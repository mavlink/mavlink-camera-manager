use super::types::*;
use super::video_source::VideoSource;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename = "mavlinkcamera")]
pub struct MavlinkCamera {
    pub definition: Definition,
    pub parameters: Parameters,
}

#[derive(Debug, Serialize)]
pub struct Definition {
    pub version: u32,
    pub model: Model,
    pub vendor: Vendor,
    //TODO: Wait for flatten to be fixed in quick-xml.
    //camera_info: CameraInfo,
}

#[derive(Debug, Serialize)]
pub struct Model {
    #[serde(rename = "$value")]
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct Vendor {
    #[serde(rename = "$value")]
    pub body: String,
}

/*
//TODO: Wait for flatten to be fixed.
#[derive(Debug, Default, Serialize)]
#[serde(tag = "")]
struct CameraInfo {
    vendor: String,
    model: String,
}*/

#[derive(Debug, Serialize)]
pub struct Parameters {
    pub parameter: Vec<ParameterType>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ParameterType {
    Bool(ParameterBool),
    Slider(ParameterSlider),
    Menu(ParameterMenu),
}

#[derive(Debug, Serialize)]
pub struct ParameterBool {
    pub name: String,
    #[serde(rename = "type")]
    pub cpp_type: String,
    pub default: i32,
    pub v4l_id: u64,
    pub description: Description,
}

#[derive(Debug, Serialize)]
pub struct ParameterSlider {
    pub name: String,
    #[serde(rename = "type")]
    pub cpp_type: String,
    pub default: i32,
    pub v4l_id: u64,
    pub step: i32,
    pub max: i32,
    pub min: i32,
    pub description: Description,
}

#[derive(Debug, Serialize)]
pub struct ParameterMenu {
    pub name: String,
    #[serde(rename = "type")]
    pub cpp_type: String,
    pub default: i32,
    pub v4l_id: u64,
    pub description: Description,
    pub options: Options,
}

#[derive(Debug, Serialize)]
pub struct Options {
    pub option: Vec<Option>,
}

#[derive(Debug, Serialize)]
pub struct Option {
    pub name: String,
    pub value: i64,
}

#[derive(Debug, Default, Serialize)]
pub struct Description {
    #[serde(rename = "$value")]
    pub body: String,
}

impl Description {
    //TODO: impl fromStr
    pub fn new(description: &str) -> Self {
        Self {
            body: description.into(),
        }
    }
}

pub fn from_video_source(video_source: &dyn VideoSource) -> String {
    let controls = video_source.controls();

    let definition = Definition {
        version: 1,
        model: Model {
            body: video_source.name().clone(),
        },
        vendor: Vendor {
            body: "Missing".into(),
        },
    };

    let parameters = controls
        .iter()
        .map(|control| match &control.configuration {
            ControlType::Bool(bool_control) => ParameterType::Bool(ParameterBool {
                name: control.id.to_string(),
                cpp_type: control.cpp_type.clone(),
                default: bool_control.default,
                v4l_id: control.id,
                description: Description::new(&control.name),
            }),
            ControlType::Slider(slider_control) => ParameterType::Slider(ParameterSlider {
                name: control.id.to_string(),
                cpp_type: control.cpp_type.clone(),
                default: slider_control.default,
                v4l_id: control.id,
                description: Description::new(&control.name),
                step: slider_control.step,
                max: slider_control.max,
                min: slider_control.min,
            }),
            ControlType::Menu(menu_control) => ParameterType::Menu(ParameterMenu {
                name: control.id.to_string(),
                cpp_type: control.cpp_type.clone(),
                default: menu_control.default,
                v4l_id: control.id,
                description: Description::new(&control.name),
                options: Options {
                    option: menu_control
                        .options
                        .iter()
                        .map(|option| Option {
                            name: option.name.clone(),
                            value: option.value,
                        })
                        .collect(),
                },
            }),
        })
        .collect();

    let mavlink_camera = MavlinkCamera {
        definition,
        parameters: Parameters {
            parameter: parameters,
        },
    };

    quick_xml::se::to_string(&mavlink_camera).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::se::to_string;

    #[test]
    fn test_device() {
        use crate::video::video_source;
        for camera in video_source::cameras_available() {
            if let VideoSourceType::Local(camera) = camera {
                let xml_string = from_video_source(&camera);
                println!("{}", xml_string);
            }
        }
    }

    #[test]
    fn deserialize() {
        let test_string = r#"<mavlinkcamera><definition version="42"><model>Potato</model><vendor>PotatoFarm</vendor></definition><parameters><parameter name="Magic Parameter Bool" type="bool" default="1" v4l_id="12345678"><description>Do magic bool stuff</description></parameter><parameter name="Magic Parameter Slider" type="int32" default="42" v4l_id="123456789" step="1" max="666" min="0"><description>Do magic slider stuff</description></parameter><parameter name="Magic Parameter Menu" type="int32" default="0" v4l_id="234567891"><description>Do magic menu stuff</description><options><option name="Magic" value="0"/><option name="Stuff" value="1"/></options></parameter></parameters></mavlinkcamera>"#;

        let struct_string = to_string(&MavlinkCamera {
            definition: Definition {
                version: 42,
                model: Model {
                    body: "Potato".into(),
                },
                vendor: Vendor {
                    body: "PotatoFarm".into(),
                },
                //TODO: Wait for flatten to be fixed.
                /*
                camera_info: CameraInfo {
                    vendor: "PotatoFarm2".into(),
                    model: "Potato2".into(),
                }*/
            },
            parameters: Parameters {
                parameter: vec![
                    ParameterType::Bool(ParameterBool {
                        name: "Magic Parameter Bool".into(),
                        cpp_type: "bool".into(),
                        default: 1,
                        v4l_id: 012345678,
                        description: Description {
                            body: "Do magic bool stuff".into(),
                        },
                    }),
                    ParameterType::Slider(ParameterSlider {
                        name: "Magic Parameter Slider".into(),
                        cpp_type: "int32".into(),
                        default: 42,
                        v4l_id: 123456789,
                        step: 1,
                        min: 0,
                        max: 666,
                        description: Description {
                            body: "Do magic slider stuff".into(),
                        },
                    }),
                    ParameterType::Menu(ParameterMenu {
                        name: "Magic Parameter Menu".into(),
                        cpp_type: "int32".into(),
                        default: 0,
                        v4l_id: 234567891,
                        description: Description {
                            body: "Do magic menu stuff".into(),
                        },
                        options: Options {
                            option: vec![
                                Option {
                                    name: "Magic".into(),
                                    value: 0,
                                },
                                Option {
                                    name: "Stuff".into(),
                                    value: 1,
                                },
                            ],
                        },
                    }),
                ],
            },
        })
        .unwrap();

        assert_eq!(test_string.to_string(), struct_string);
    }
}
