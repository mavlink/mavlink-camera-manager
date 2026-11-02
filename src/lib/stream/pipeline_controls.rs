use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{
    controls::{
        gst_element_controls::{
            PIPELINE_CONTROL_ID_OFFSET, enum_value_by_nick, flags_bits_by_nick, float_to_api,
            list_controls_on_live_element, list_decoder_controls, list_encoder_controls,
            pipeline_control_id_for_element_property, set_property_from_api,
        },
        types::{Control, ControlBool, ControlMenu, ControlState, ControlType},
    },
    stream::{
        Stream,
        types::{CaptureConfiguration, PropertyValue, SourceConfiguration},
    },
    video::types::VideoEncodeType,
    video_stream::types::VideoAndStreamInformation,
};
use anyhow::{Context, Result, anyhow};
use glib::prelude::*;
use gst::prelude::*;

const SYNTHETIC_ELEMENT: &str = "stream";
const MANUAL_ENCODER_ELEMENT: &str = "encoder";
const MANUAL_DECODER_ELEMENT: &str = "decoder";
const AUTO_RESTART_ON_CONFIG_CHANGE: &str = "auto-restart-on-config-change";
const RESTART_STREAM: &str = "restart-stream";
const AUTO_BIN_ELEMENT_NAMES: &[&str] = &["encodebin", "decodebin", "transcodebin"];

pub fn auto_restart_control_id() -> u64 {
    pipeline_control_id_for_element_property(SYNTHETIC_ELEMENT, AUTO_RESTART_ON_CONFIG_CHANGE)
}

pub fn restart_stream_control_id() -> u64 {
    pipeline_control_id_for_element_property(SYNTHETIC_ELEMENT, RESTART_STREAM)
}

pub fn is_pipeline_control_id(control_id: u64) -> bool {
    control_id >= PIPELINE_CONTROL_ID_OFFSET
}

pub fn list_pipeline_controls(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Vec<Control> {
    let codec_controls = list_codec_controls_for_stream(stream, video_and_stream_information);
    let synthetic_controls = list_synthetic_controls(video_and_stream_information);
    let mut controls = codec_controls
        .into_iter()
        .chain(synthetic_controls)
        .filter(|control| !control.element.is_empty())
        .collect::<Vec<_>>();
    sort_pipeline_controls(&mut controls);
    controls
}

pub fn pipeline_controls_for_mavlink(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Vec<Control> {
    list_pipeline_controls(stream, video_and_stream_information)
        .into_iter()
        .filter(|control| control.cpp_type != "string")
        .collect()
}

pub fn pipeline_control_value_by_id(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
    control_id: u64,
) -> std::io::Result<i64> {
    let Some(control) = find_pipeline_control(stream, video_and_stream_information, control_id)
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Pipeline control {control_id} not found"),
        ));
    };
    Ok(control_current_value(&control.configuration))
}

/// Returns true when the caller must restart the stream after writing this
/// configuration back to the live `Stream` (so rebuild sees the new values).
pub fn set_pipeline_control(
    stream: &Stream,
    video_and_stream_information: &mut VideoAndStreamInformation,
    control_id: u64,
    value: i64,
) -> Result<bool> {
    if control_id == restart_stream_control_id() {
        return Ok(value != 0);
    }

    if control_id == auto_restart_control_id() {
        set_auto_restart_on_config_change(video_and_stream_information, value != 0);
        return Ok(false);
    }

    let control = find_pipeline_control(stream, video_and_stream_information, control_id)
        .context("Pipeline control not found")?;

    if control.mutable_in_playing {
        if let Some(element) = try_live_codec_element(stream, &control.element) {
            set_property_from_api(&element, &control.name, value).map_err(|error| {
                anyhow!(
                    "Failed setting live codec property {control_name} on {element_name}: {error}",
                    control_name = control.name,
                    element_name = control.element
                )
            })?;
        }
    }

    let probe_factory =
        probe_factory_name_for_control(video_and_stream_information, &control.element);
    let probe_element = gst::ElementFactory::make(&probe_factory).build().ok();
    let property_value = api_value_to_property_value(&control, value, probe_element.as_ref());

    match control.element.as_str() {
        MANUAL_ENCODER_ELEMENT => {
            update_encoder_property(video_and_stream_information, &control.name, property_value)?;
        }
        MANUAL_DECODER_ELEMENT => {
            update_decoder_property(video_and_stream_information, &control.name, property_value)?;
        }
        _ => {}
    }

    if control.requires_restart {
        if auto_restart_on_config_change(video_and_stream_information) {
            return Ok(true);
        }
        stream.set_restart_needed(true);
    }

    Ok(false)
}

pub fn reset_pipeline_controls(
    stream: &Stream,
    video_and_stream_information: &mut VideoAndStreamInformation,
) {
    if let CaptureConfiguration::Video(video_configuration) = &mut video_and_stream_information
        .stream_information
        .configuration
    {
        match &mut video_configuration.source_configuration {
            SourceConfiguration::ManualTranscoding(manual_config) => {
                manual_config.encoder_properties.clear();
                manual_config.decoder_properties.clear();
            }
            SourceConfiguration::AutoTranscoding(auto_config) => {
                auto_config.encoder_properties.clear();
                auto_config.decoder_properties.clear();
            }
            SourceConfiguration::Classic => {}
        }
        video_configuration.auto_restart_on_config_change = false;
    }

    stream.set_restart_needed(true);
}

fn list_codec_controls_for_stream(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
) -> Vec<Control> {
    let CaptureConfiguration::Video(video_configuration) = &video_and_stream_information
        .stream_information
        .configuration
    else {
        return vec![];
    };

    match &video_configuration.source_configuration {
        SourceConfiguration::Classic => vec![],
        SourceConfiguration::ManualTranscoding(manual_config) => list_manual_transcoding_controls(
            stream,
            video_and_stream_information,
            video_configuration,
            manual_config,
        ),
        SourceConfiguration::AutoTranscoding(auto_config) => list_auto_transcoding_controls(
            stream,
            video_and_stream_information,
            video_configuration,
            auto_config,
        ),
    }
}

fn list_manual_transcoding_controls(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
    video_configuration: &crate::stream::types::VideoCaptureConfiguration,
    manual_config: &crate::stream::types::ManualTranscodingConfig,
) -> Vec<Control> {
    let mut controls = Vec::new();

    if is_compressed_source(&video_configuration.source_encode) {
        if let Ok(factory_name) = decoder_factory_name(video_and_stream_information) {
            controls.extend(list_manual_decoder_controls(
                stream,
                &factory_name,
                &manual_config.decoder_properties,
            ));
        }
    }

    if !is_raw_encode(&video_configuration.sink_encode) {
        controls.extend(list_manual_encoder_controls(
            stream,
            video_and_stream_information,
            manual_config,
        ));
    }
    controls
}

fn list_manual_decoder_controls(
    stream: &Stream,
    factory_name: &str,
    decoder_properties: &std::collections::BTreeMap<String, PropertyValue>,
) -> Vec<Control> {
    let probe_element = gst::ElementFactory::make(factory_name).build().ok();
    let mut controls = list_decoder_controls(factory_name);
    overlay_element_name(&mut controls, MANUAL_DECODER_ELEMENT);
    overlay_element_properties(&mut controls, decoder_properties, probe_element.as_ref());
    if let Some(decoder) = try_live_decoder_element(stream) {
        overlay_live_values(&mut controls, &decoder);
    }
    controls
}

fn list_manual_encoder_controls(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
    manual_config: &crate::stream::types::ManualTranscodingConfig,
) -> Vec<Control> {
    let factory_name = encoder_factory_name(video_and_stream_information);
    let probe_element = gst::ElementFactory::make(&factory_name).build().ok();
    let mut controls = list_encoder_controls(&factory_name);
    overlay_element_name(&mut controls, MANUAL_ENCODER_ELEMENT);
    #[cfg(target_os = "linux")]
    overlay_element_properties(
        &mut controls,
        &crate::stream::pipeline::transcoding::startup_encoder_properties(&factory_name),
        probe_element.as_ref(),
    );
    overlay_element_properties(
        &mut controls,
        &manual_config.encoder_properties,
        probe_element.as_ref(),
    );
    if let Some(encoder) = try_live_encoder_element(stream) {
        overlay_live_values(&mut controls, &encoder);
    }
    controls
}

fn list_auto_transcoding_controls(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
    video_configuration: &crate::stream::types::VideoCaptureConfiguration,
    auto_config: &crate::stream::types::AutoTranscodingConfig,
) -> Vec<Control> {
    if let Some(elements) = try_live_autobin_codec_elements(stream) {
        if !elements.is_empty() {
            return elements
                .iter()
                .flat_map(|element| {
                    let mut controls = list_controls_on_live_element(element);
                    overlay_element_name(&mut controls, codec_role_element_name(element).as_str());
                    controls
                })
                .collect();
        }
    }

    let raw_source = is_raw_encode(&video_configuration.source_encode);
    let compressed_source = is_compressed_source(&video_configuration.source_encode);
    let raw_sink = is_raw_encode(&video_configuration.sink_encode);
    let compressed_sink = is_compressed_source(&video_configuration.sink_encode);

    let mut controls = Vec::new();
    if compressed_source && (raw_sink || compressed_sink) {
        if let Some(factory_name) = default_decoder_factory_name(&video_configuration.source_encode)
        {
            let probe_element = gst::ElementFactory::make(&factory_name).build().ok();
            let mut decoder_controls = list_decoder_controls(&factory_name);
            overlay_element_name(&mut decoder_controls, MANUAL_DECODER_ELEMENT);
            overlay_element_properties(
                &mut decoder_controls,
                &auto_config.decoder_properties,
                probe_element.as_ref(),
            );
            controls.extend(decoder_controls);
        }
    }
    if (raw_source && compressed_sink) || (compressed_source && compressed_sink) {
        let factory_name = encoder_factory_name(video_and_stream_information);
        let probe_element = gst::ElementFactory::make(&factory_name).build().ok();
        let mut encoder_controls = list_encoder_controls(&factory_name);
        overlay_element_name(&mut encoder_controls, MANUAL_ENCODER_ELEMENT);
        overlay_element_properties(
            &mut encoder_controls,
            &auto_config.encoder_properties,
            probe_element.as_ref(),
        );
        controls.extend(encoder_controls);
    }
    controls
}

fn codec_role_element_name(element: &gst::Element) -> String {
    if element
        .factory()
        .map(|factory| factory.klass().contains("Decoder"))
        .unwrap_or(false)
    {
        MANUAL_DECODER_ELEMENT.to_string()
    } else {
        MANUAL_ENCODER_ELEMENT.to_string()
    }
}

fn list_synthetic_controls(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Vec<Control> {
    let auto_restart = auto_restart_on_config_change(video_and_stream_information);
    vec![
        synthetic_bool_control(
            AUTO_RESTART_ON_CONFIG_CHANGE,
            "Auto restart on config change",
            "When enabled, changing a control that requires restart immediately restarts the stream.",
            auto_restart_control_id(),
            auto_restart,
            false,
        ),
        synthetic_bool_control(
            RESTART_STREAM,
            "Restart stream",
            "Rebuild the pipeline so pending encoder changes take effect.",
            restart_stream_control_id(),
            false,
            true,
        ),
    ]
}

fn synthetic_bool_control(
    name: &str,
    nick: &str,
    blurb: &str,
    id: u64,
    value: bool,
    write_only: bool,
) -> Control {
    let value = i64::from(value);
    Control {
        name: name.to_string(),
        element: SYNTHETIC_ELEMENT.to_string(),
        cpp_type: "bool".to_string(),
        id,
        state: ControlState::default(),
        configuration: ControlType::Bool(ControlBool {
            default: 0,
            value: if write_only { 0 } else { value },
        }),
        mutable_in_playing: true,
        requires_restart: false,
        nick: nick.to_string(),
        blurb: Some(blurb.to_string()),
        docs_url: None,
        plugin_name: Some(SYNTHETIC_ELEMENT.to_string()),
    }
}

fn find_pipeline_control(
    stream: &Stream,
    video_and_stream_information: &VideoAndStreamInformation,
    control_id: u64,
) -> Option<Control> {
    list_pipeline_controls(stream, video_and_stream_information)
        .into_iter()
        .find(|control| control.id == control_id)
}

fn is_raw_encode(encode: &VideoEncodeType) -> bool {
    matches!(
        encode,
        VideoEncodeType::Nv12 | VideoEncodeType::Yuyv | VideoEncodeType::Rgb
    )
}

fn is_compressed_source(source_encode: &VideoEncodeType) -> bool {
    matches!(
        source_encode,
        VideoEncodeType::Mjpg | VideoEncodeType::H264 | VideoEncodeType::H265
    )
}

pub fn decoder_factory_name(
    video_and_stream_information: &VideoAndStreamInformation,
) -> Result<String> {
    let CaptureConfiguration::Video(video_configuration) = &video_and_stream_information
        .stream_information
        .configuration
    else {
        return Err(anyhow!("Stream configuration is not video capture"));
    };

    match &video_configuration.source_configuration {
        SourceConfiguration::ManualTranscoding(manual_config) => {
            if !manual_config.decoder.is_empty() {
                return Ok(manual_config.decoder.clone());
            }
        }
        SourceConfiguration::AutoTranscoding(_) => {}
        _ => return Err(anyhow!("Stream is not using transcoding with a decoder")),
    }

    default_decoder_factory_name(&video_configuration.source_encode)
        .ok_or_else(|| anyhow!("Source encode does not require a decoder"))
}

fn default_decoder_factory_name(source_encode: &VideoEncodeType) -> Option<String> {
    match source_encode {
        VideoEncodeType::Mjpg => Some("jpegdec".to_string()),
        VideoEncodeType::H264 => Some("avdec_h264".to_string()),
        VideoEncodeType::H265 => Some("avdec_h265".to_string()),
        _ => None,
    }
}

fn encoder_factory_name(video_and_stream_information: &VideoAndStreamInformation) -> String {
    let CaptureConfiguration::Video(video_configuration) = &video_and_stream_information
        .stream_information
        .configuration
    else {
        return crate::stream::gst::encoding::preferred_encoder_factory_name(
            &VideoEncodeType::H264,
        )
        .to_string();
    };

    match &video_configuration.source_configuration {
        SourceConfiguration::ManualTranscoding(manual_config)
            if !manual_config.encoder.is_empty() =>
        {
            manual_config.encoder.clone()
        }
        _ => crate::stream::gst::encoding::preferred_encoder_factory_name(
            &video_configuration.sink_encode,
        )
        .to_string(),
    }
}

fn probe_factory_name_for_control(
    video_and_stream_information: &VideoAndStreamInformation,
    control_element: &str,
) -> String {
    match control_element {
        MANUAL_ENCODER_ELEMENT => encoder_factory_name(video_and_stream_information),
        MANUAL_DECODER_ELEMENT => decoder_factory_name(video_and_stream_information)
            .unwrap_or_else(|_| "jpegdec".to_string()),
        element_name => gst::ElementFactory::find(element_name)
            .map(|factory| factory.name().to_string())
            .unwrap_or_else(|| element_name.to_string()),
    }
}

fn auto_restart_on_config_change(video_and_stream_information: &VideoAndStreamInformation) -> bool {
    let CaptureConfiguration::Video(video_configuration) = &video_and_stream_information
        .stream_information
        .configuration
    else {
        return false;
    };
    video_configuration.auto_restart_on_config_change
}

fn set_auto_restart_on_config_change(
    video_and_stream_information: &mut VideoAndStreamInformation,
    value: bool,
) {
    if let CaptureConfiguration::Video(video_configuration) = &mut video_and_stream_information
        .stream_information
        .configuration
    {
        video_configuration.auto_restart_on_config_change = value;
    }
}

fn update_encoder_property(
    video_and_stream_information: &mut VideoAndStreamInformation,
    property_name: &str,
    property_value: PropertyValue,
) -> Result<()> {
    let CaptureConfiguration::Video(video_configuration) = &mut video_and_stream_information
        .stream_information
        .configuration
    else {
        return Err(anyhow!("Stream configuration is not video capture"));
    };

    match &mut video_configuration.source_configuration {
        SourceConfiguration::ManualTranscoding(manual_config) => {
            manual_config
                .encoder_properties
                .insert(property_name.to_string(), property_value);
        }
        SourceConfiguration::AutoTranscoding(auto_config) => {
            auto_config
                .encoder_properties
                .insert(property_name.to_string(), property_value);
        }
        _ => return Err(anyhow!("Stream is not using transcoding with an encoder")),
    }
    Ok(())
}

fn update_decoder_property(
    video_and_stream_information: &mut VideoAndStreamInformation,
    property_name: &str,
    property_value: PropertyValue,
) -> Result<()> {
    let CaptureConfiguration::Video(video_configuration) = &mut video_and_stream_information
        .stream_information
        .configuration
    else {
        return Err(anyhow!("Stream configuration is not video capture"));
    };

    match &mut video_configuration.source_configuration {
        SourceConfiguration::ManualTranscoding(manual_config) => {
            manual_config
                .decoder_properties
                .insert(property_name.to_string(), property_value);
        }
        SourceConfiguration::AutoTranscoding(auto_config) => {
            auto_config
                .decoder_properties
                .insert(property_name.to_string(), property_value);
        }
        _ => return Err(anyhow!("Stream is not using transcoding with a decoder")),
    }
    Ok(())
}

fn overlay_element_name(controls: &mut [Control], element_name: &str) {
    for control in controls {
        control.element = element_name.to_string();
    }
}

fn overlay_element_properties(
    controls: &mut [Control],
    element_properties: &std::collections::BTreeMap<String, PropertyValue>,
    probe_element: Option<&gst::Element>,
) {
    for control in controls {
        let Some(property_value) = element_properties.get(&control.name) else {
            continue;
        };
        let api_value = property_value_to_api(property_value, control, probe_element);
        set_control_value(control, api_value);
    }
}

fn overlay_live_values(controls: &mut [Control], element: &gst::Element) {
    for control in controls {
        if let Some(value) = read_property_as_api(element, &control.name) {
            set_control_value(control, value);
        }
    }
}

fn property_value_to_api(
    property_value: &PropertyValue,
    control: &Control,
    probe_element: Option<&gst::Element>,
) -> i64 {
    match property_value {
        PropertyValue::Bool(boolean) => i64::from(*boolean),
        PropertyValue::Integer(integer) => *integer,
        PropertyValue::Number(number) => float_to_api(*number),
        PropertyValue::String(string) => probe_element
            .and_then(|element| {
                enum_value_by_nick(element, &control.name, string)
                    .or_else(|| flags_bits_by_nick(element, &control.name, string))
            })
            .unwrap_or(0),
    }
}

fn api_value_to_property_value(
    control: &Control,
    value: i64,
    probe_element: Option<&gst::Element>,
) -> PropertyValue {
    match &control.configuration {
        ControlType::Bool(_) => PropertyValue::Bool(value != 0),
        ControlType::Menu(ControlMenu { options, .. }) => {
            if let Some(option) = options.iter().find(|option| option.value == value) {
                PropertyValue::String(option.name.clone())
            } else if let Some(element) = probe_element
                && let Some(param_spec) = element.find_property(&control.name)
                && let Some(enum_class) = glib::EnumClass::with_type(param_spec.value_type())
                && let Ok(enum_int) = i32::try_from(value)
                && let Some(enum_value) = enum_class
                    .values()
                    .iter()
                    .find(|enum_value| enum_value.value() == enum_int)
            {
                PropertyValue::String(enum_value.nick().to_string())
            } else {
                PropertyValue::Integer(value)
            }
        }
        ControlType::Slider(_) | ControlType::Flags(_) => PropertyValue::Integer(value),
    }
}

fn set_control_value(control: &mut Control, value: i64) {
    match &mut control.configuration {
        ControlType::Bool(bool_control) => bool_control.value = value,
        ControlType::Slider(slider) => slider.value = value,
        ControlType::Menu(menu) => menu.value = value,
        ControlType::Flags(flags) => flags.value = value,
    }
}

fn control_current_value(configuration: &ControlType) -> i64 {
    match configuration {
        ControlType::Bool(control) => control.value,
        ControlType::Slider(control) => control.value,
        ControlType::Menu(control) => control.value,
        ControlType::Flags(control) => control.value,
    }
}

fn try_live_codec_element(stream: &Stream, element_name: &str) -> Option<gst::Element> {
    match element_name {
        MANUAL_ENCODER_ELEMENT => try_live_encoder_element(stream),
        MANUAL_DECODER_ELEMENT => try_live_decoder_element(stream),
        _ => try_live_named_element(stream, element_name)
            .or_else(|| try_live_autobin_element_by_name(stream, element_name)),
    }
}

fn try_live_encoder_element(stream: &Stream) -> Option<gst::Element> {
    let state_guard = stream.state.try_read().ok()?;
    let state = state_guard.as_ref()?;
    let pipeline = state.pipeline.as_ref()?;
    pipeline
        .inner_state_as_ref()
        .pipeline
        .by_name(MANUAL_ENCODER_ELEMENT)
}

fn try_live_decoder_element(stream: &Stream) -> Option<gst::Element> {
    let state_guard = stream.state.try_read().ok()?;
    let state = state_guard.as_ref()?;
    let pipeline = state.pipeline.as_ref()?;
    pipeline
        .inner_state_as_ref()
        .pipeline
        .by_name(MANUAL_DECODER_ELEMENT)
}

fn try_live_named_element(stream: &Stream, element_name: &str) -> Option<gst::Element> {
    let state_guard = stream.state.try_read().ok()?;
    let state = state_guard.as_ref()?;
    let pipeline = state.pipeline.as_ref()?;
    pipeline.inner_state_as_ref().pipeline.by_name(element_name)
}

fn try_live_autobin_element_by_name(stream: &Stream, element_name: &str) -> Option<gst::Element> {
    try_live_autobin_codec_elements(stream)?
        .into_iter()
        .find(|element| element.name() == element_name)
}

fn try_live_autobin_codec_elements(stream: &Stream) -> Option<Vec<gst::Element>> {
    let state_guard = stream.state.try_read().ok()?;
    let state = state_guard.as_ref()?;
    let pipeline = state.pipeline.as_ref()?;
    for bin_name in AUTO_BIN_ELEMENT_NAMES {
        if let Some(autobin) = pipeline.inner_state_as_ref().pipeline.by_name(bin_name) {
            return Some(collect_autobin_codec_elements(&autobin));
        }
    }
    None
}

fn collect_autobin_codec_elements(parent: &gst::Element) -> Vec<gst::Element> {
    let Ok(bin) = parent.clone().downcast::<gst::Bin>() else {
        return vec![];
    };
    let mut elements = bin
        .iterate_recurse()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|element| {
            element
                .factory()
                .map(|factory| {
                    let klass = factory.klass();
                    klass.contains("Decoder") || klass.contains("Encoder")
                })
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    elements.sort_by_key(codec_element_sort_group);
    elements
}

fn codec_element_sort_group(element: &gst::Element) -> u8 {
    element
        .factory()
        .map(|factory| {
            let klass = factory.klass();
            if klass.contains("Decoder") {
                0
            } else if klass.contains("Encoder") {
                1
            } else {
                2
            }
        })
        .unwrap_or(2)
}

fn sort_pipeline_controls(controls: &mut [Control]) {
    controls.sort_by(|left, right| {
        left.plugin_name
            .as_deref()
            .unwrap_or("")
            .cmp(right.plugin_name.as_deref().unwrap_or(""))
            .then_with(|| pipeline_control_sort_key(left).cmp(&pipeline_control_sort_key(right)))
            .then_with(|| left.element.cmp(&right.element))
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn pipeline_control_sort_key(control: &Control) -> u8 {
    match control.element.as_str() {
        MANUAL_DECODER_ELEMENT => 0,
        element if element.contains("dec") => 0,
        MANUAL_ENCODER_ELEMENT => 1,
        element if element.contains("enc") => 1,
        SYNTHETIC_ELEMENT => 3,
        _ => 2,
    }
}

fn read_property_as_api(element: &gst::Element, property: &str) -> Option<i64> {
    catch_unwind(AssertUnwindSafe(|| {
        let param_spec = element.find_property(property)?;
        let value = element.property_value(property);
        if let Some((_enum_class, enum_value)) = glib::EnumValue::from_value(&value) {
            return Some(i64::from(enum_value.value()));
        }
        if let Some((_flags_class, flags)) = glib::FlagsValue::from_value(&value) {
            return Some(i64::from(
                flags.iter().fold(0u32, |bits, flag| bits | flag.value()),
            ));
        }

        let value_type = param_spec.value_type();
        if value_type == bool::static_type() {
            return value.get::<bool>().ok().map(|boolean| i64::from(boolean));
        }
        if value_type == i32::static_type() {
            return value.get::<i32>().ok().map(i64::from);
        }
        if value_type == u32::static_type() {
            return value.get::<u32>().ok().map(|unsigned| i64::from(unsigned));
        }
        if value_type == i64::static_type() {
            return value.get::<i64>().ok();
        }
        if value_type == u64::static_type() {
            return value
                .get::<u64>()
                .ok()
                .and_then(|unsigned| i64::try_from(unsigned).ok());
        }
        if value_type == f32::static_type() {
            return value
                .get::<f32>()
                .ok()
                .map(|float| float_to_api(f64::from(float)));
        }
        if value_type == f64::static_type() {
            return value.get::<f64>().ok().map(float_to_api);
        }
        None
    }))
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::gst_element_controls::PIPELINE_CONTROL_ID_OFFSET;

    #[test]
    fn pipeline_control_ids_are_in_pipeline_namespace() {
        let bitrate_id = pipeline_control_id_for_element_property("x264enc", "bitrate");
        assert!(bitrate_id >= PIPELINE_CONTROL_ID_OFFSET);
    }

    #[test]
    fn synthetic_control_ids_are_in_pipeline_namespace() {
        assert!(auto_restart_control_id() >= PIPELINE_CONTROL_ID_OFFSET);
        assert!(restart_stream_control_id() >= PIPELINE_CONTROL_ID_OFFSET);
        assert_ne!(auto_restart_control_id(), restart_stream_control_id());
    }

    #[test]
    fn x264enc_enum_and_flags_round_trip_as_api() {
        let _ = gst::init();
        let encoder = gst::ElementFactory::make("x264enc")
            .build()
            .expect("x264enc");
        let speed_preset =
            enum_value_by_nick(&encoder, "speed-preset", "ultrafast").expect("ultrafast nick");
        let tune = flags_bits_by_nick(&encoder, "tune", "zerolatency").expect("zerolatency nick");
        set_property_from_api(&encoder, "speed-preset", speed_preset).unwrap();
        set_property_from_api(&encoder, "tune", tune).unwrap();
        assert_eq!(
            read_property_as_api(&encoder, "speed-preset"),
            Some(speed_preset)
        );
        assert_eq!(read_property_as_api(&encoder, "tune"), Some(tune));
        assert_ne!(tune, 0);
    }

    #[test]
    fn default_decoder_factory_names_match_source_encode() {
        assert_eq!(
            default_decoder_factory_name(&VideoEncodeType::Mjpg),
            Some("jpegdec".to_string())
        );
        assert_eq!(
            default_decoder_factory_name(&VideoEncodeType::H264),
            Some("avdec_h264".to_string())
        );
        assert_eq!(
            default_decoder_factory_name(&VideoEncodeType::H265),
            Some("avdec_h265".to_string())
        );
        assert_eq!(default_decoder_factory_name(&VideoEncodeType::Nv12), None);
    }

    #[test]
    fn list_decoder_controls_discovers_jpegdec_properties() {
        let _ = gst::init();
        if gst::ElementFactory::find("jpegdec").is_none() {
            return;
        }

        let controls = list_decoder_controls("jpegdec");
        assert!(
            !controls.is_empty(),
            "jpegdec should expose at least one controllable property"
        );
        assert!(
            controls.iter().all(|control| control.element == "jpegdec"),
            "factory listing keeps factory name as element until overlay"
        );
    }

    #[test]
    fn overlay_element_name_rewrites_control_element_field() {
        let _ = gst::init();
        if gst::ElementFactory::find("x264enc").is_none() {
            return;
        }

        let mut controls = list_encoder_controls("x264enc");
        assert!(controls.iter().all(|control| control.element == "x264enc"));
        overlay_element_name(&mut controls, MANUAL_ENCODER_ELEMENT);
        assert!(
            controls
                .iter()
                .all(|control| control.element == MANUAL_ENCODER_ELEMENT)
        );
    }

    #[test]
    fn decoder_and_encoder_pipeline_control_ids_use_element_namespace() {
        let encoder_id = pipeline_control_id_for_element_property("encoder", "bitrate");
        let decoder_id = pipeline_control_id_for_element_property("decoder", "max-errors");
        assert!(encoder_id >= PIPELINE_CONTROL_ID_OFFSET);
        assert!(decoder_id >= PIPELINE_CONTROL_ID_OFFSET);
        assert_ne!(encoder_id, decoder_id);
    }
}
