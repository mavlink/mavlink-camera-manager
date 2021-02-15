use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(rename = "mavlinkcamera")]
struct MavlinkCamera {
    definition: Definition,
    parameters: Parameters,
}

#[derive(Debug, Serialize)]
struct Definition {
    version: u32,
    model: Model,
    vendor: Vendor,
    //TODO: Wait for flatten to be fixed in quick-xml.
    //camera_info: CameraInfo,
}

#[derive(Debug, Serialize)]
struct Model {
    #[serde(rename = "$value")]
    body: String,
}

#[derive(Debug, Serialize)]
struct Vendor {
    #[serde(rename = "$value")]
    body: String,
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
struct Parameters {
    parameter: Vec<ParameterType>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ParameterType {
    Bool(ParameterBool),
    Slider(ParameterSlider),
    Menu(ParameterMenu),
}

#[derive(Debug, Serialize)]
struct ParameterBool {
    name: String,
    #[serde(rename = "type")]
    cpp_type: String,
    default: i32,
    v4l2_id: i64,
    description: Description,
}

#[derive(Debug, Serialize)]
struct ParameterSlider {
    name: String,
    #[serde(rename = "type")]
    cpp_type: String,
    default: i32,
    v4l2_id: i64,
    step: u32,
    max: u32,
    min: u32,
    description: Description,
}

#[derive(Debug, Serialize)]
struct ParameterMenu {
    name: String,
    #[serde(rename = "type")]
    cpp_type: String,
    default: i32,
    v4l2_id: i64,
    description: Description,
    options: Options,
}

#[derive(Debug, Serialize)]
struct Options {
    option: Vec<Option>,
}

#[derive(Debug, Serialize)]
struct Option {
    name: String,
    value: i32,
}

#[derive(Debug, Serialize)]
struct Description {
    #[serde(rename = "$value")]
    body: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::se::to_string;

    #[test]
    fn deserialize() {
        let test_string = r#"<mavlinkcamera><definition version="42"><model>Potato</model><vendor>PotatoFarm</vendor></definition><parameters><parameter name="Magic Parameter Bool" type="bool" default="1" v4l2_id="12345678"><description>Do magic bool stuff</description></parameter><parameter name="Magic Parameter Slider" type="int32" default="42" v4l2_id="123456789" step="1" max="666" min="0"><description>Do magic slider stuff</description></parameter><parameter name="Magic Parameter Menu" type="int32" default="0" v4l2_id="234567891"><description>Do magic menu stuff</description><options><option name="Magic" value="0"/><option name="Stuff" value="1"/></options></parameter></parameters></mavlinkcamera>"#;

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
                        v4l2_id: 012345678,
                        description: Description {
                            body: "Do magic bool stuff".into(),
                        },
                    }),
                    ParameterType::Slider(ParameterSlider {
                        name: "Magic Parameter Slider".into(),
                        cpp_type: "int32".into(),
                        default: 42,
                        v4l2_id: 123456789,
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
                        v4l2_id: 234567891,
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

        assert_eq!(test_string.to_string(), struct_string.to_string());
    }
}
