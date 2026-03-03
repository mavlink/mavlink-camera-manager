use url::Url;
use uuid::Uuid;

use super::controls::{
    Control, ControlBool, ControlMenu, ControlOption, ControlSlider, ControlState, ControlType,
};
use super::server::{
    ApiVideoSource, AuthenticateOnvifDeviceRequest, BlockSource, Development, Info, OnvifDevice,
    PostStream, RemoveStream, ResetCameraControls, ResetSettings, SdpFileRequest,
    ThumbnailFileRequest, UnauthenticateOnvifDeviceRequest, UnblockSource, V4lControl,
    XmlFileRequest,
};
use super::signalling::{
    Answer, BindAnswer, BindOffer, IceNegotiation, MediaNegotiation, Message, Negotiation,
    PeerIdAnswer, Question, RTCIceCandidateInit, RTCSessionDescription, Sdp, Stream,
};
use super::stream::{
    CaptureConfiguration, ExtendedConfiguration, MavlinkComponent, StreamInformation, StreamStatus,
    VideoAndStreamInformation, VideoCaptureConfiguration,
};
use super::video::{
    Format, FrameInterval, OnvifDeviceInformation, Size, VideoEncodeType, VideoSourceGst,
    VideoSourceGstType, VideoSourceLocal, VideoSourceLocalType, VideoSourceOnvif,
    VideoSourceOnvifType, VideoSourceRedirect, VideoSourceRedirectType, VideoSourceType,
};

// ---------------------------------------------------------------------------
// Helper constructors
// ---------------------------------------------------------------------------

fn sample_frame_interval() -> FrameInterval {
    FrameInterval {
        numerator: 1,
        denominator: 30,
    }
}

fn sample_size() -> Size {
    Size {
        width: 1920,
        height: 1080,
        intervals: vec![sample_frame_interval()],
    }
}

fn sample_format() -> Format {
    Format {
        encode: VideoEncodeType::H264,
        sizes: vec![sample_size()],
    }
}

fn sample_video_source_local() -> VideoSourceLocal {
    VideoSourceLocal {
        name: "USB Camera".into(),
        device_path: "/dev/video0".into(),
        typ: VideoSourceLocalType::Usb("usb-0000:00:14.0-1".into()),
    }
}

fn sample_onvif_device_information() -> OnvifDeviceInformation {
    OnvifDeviceInformation {
        manufacturer: "ACME Corp".into(),
        model: "IPCam-4K".into(),
        firmware_version: "2.1.0".into(),
        serial_number: "SN-123456".into(),
        hardware_id: "HW-789".into(),
    }
}

fn sample_video_source_gst() -> VideoSourceGst {
    VideoSourceGst {
        name: "Fake source".into(),
        source: VideoSourceGstType::Fake("videotestsrc".into()),
    }
}

fn sample_video_source_onvif() -> VideoSourceOnvif {
    VideoSourceOnvif {
        name: "ONVIF Camera".into(),
        source: VideoSourceOnvifType::Onvif("onvif://192.168.1.100".into()),
        device_information: sample_onvif_device_information(),
    }
}

fn sample_video_source_redirect() -> VideoSourceRedirect {
    VideoSourceRedirect {
        name: "RTSP Redirect".into(),
        source: VideoSourceRedirectType::Redirect("rtsp://192.168.1.200:554/stream".into()),
    }
}

fn sample_video_capture_configuration() -> VideoCaptureConfiguration {
    VideoCaptureConfiguration {
        encode: VideoEncodeType::H264,
        height: 1080,
        width: 1920,
        frame_interval: sample_frame_interval(),
    }
}

fn sample_extended_configuration() -> ExtendedConfiguration {
    ExtendedConfiguration {
        thermal: true,
        disable_mavlink: false,
        disable_zenoh: false,
        disable_thumbnails: false,
    }
}

fn sample_stream_information() -> StreamInformation {
    StreamInformation {
        endpoints: vec![Url::parse("rtsp://0.0.0.0:8554/test").unwrap()],
        configuration: CaptureConfiguration::Video(sample_video_capture_configuration()),
        extended_configuration: Some(sample_extended_configuration()),
    }
}

fn sample_video_and_stream_information() -> VideoAndStreamInformation {
    VideoAndStreamInformation {
        name: "Test Stream".into(),
        stream_information: sample_stream_information(),
        video_source: VideoSourceType::Local(sample_video_source_local()),
    }
}

fn sample_bind_answer() -> BindAnswer {
    BindAnswer {
        consumer_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        producer_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
        session_id: Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap(),
    }
}

// ===========================================================================
// video.rs — bidirectional types (roundtrip + snapshot)
// ===========================================================================

#[test]
fn roundtrip_video_encode_type_h264() {
    let original = VideoEncodeType::H264;
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoEncodeType = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn roundtrip_video_encode_type_unknown() {
    let original = VideoEncodeType::Unknown("AV1".into());
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoEncodeType = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_encode_type_variants() {
    insta::assert_json_snapshot!("video_encode_h264", VideoEncodeType::H264);
    insta::assert_json_snapshot!("video_encode_h265", VideoEncodeType::H265);
    insta::assert_json_snapshot!("video_encode_mjpg", VideoEncodeType::Mjpg);
    insta::assert_json_snapshot!("video_encode_rgb", VideoEncodeType::Rgb);
    insta::assert_json_snapshot!("video_encode_yuyv", VideoEncodeType::Yuyv);
    insta::assert_json_snapshot!(
        "video_encode_unknown",
        VideoEncodeType::Unknown("AV1".into())
    );
}

#[test]
fn roundtrip_frame_interval() {
    let original = sample_frame_interval();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: FrameInterval = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_frame_interval() {
    insta::assert_json_snapshot!(sample_frame_interval());
}

#[test]
fn roundtrip_size() {
    let original = sample_size();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: Size = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_size() {
    insta::assert_json_snapshot!(sample_size());
}

#[test]
fn roundtrip_format() {
    let original = sample_format();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: Format = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_format() {
    insta::assert_json_snapshot!(sample_format());
}

#[test]
fn roundtrip_video_source_local_type() {
    let original = VideoSourceLocalType::Usb("usb-0000".into());
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceLocalType = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn roundtrip_video_source_local() {
    let original = sample_video_source_local();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceLocal = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_source_local() {
    insta::assert_json_snapshot!(sample_video_source_local());
}

#[test]
fn roundtrip_video_source_gst() {
    let original = sample_video_source_gst();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceGst = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_source_gst() {
    insta::assert_json_snapshot!(sample_video_source_gst());
}

#[test]
fn roundtrip_video_source_onvif() {
    let original = sample_video_source_onvif();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceOnvif = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_source_onvif() {
    insta::assert_json_snapshot!(sample_video_source_onvif());
}

#[test]
fn roundtrip_onvif_device_information() {
    let original = sample_onvif_device_information();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: OnvifDeviceInformation = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_onvif_device_information() {
    insta::assert_json_snapshot!(sample_onvif_device_information());
}

#[test]
fn roundtrip_video_source_redirect() {
    let original = sample_video_source_redirect();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceRedirect = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_source_redirect() {
    insta::assert_json_snapshot!(sample_video_source_redirect());
}

#[test]
fn roundtrip_video_source_type_local() {
    let original = VideoSourceType::Local(sample_video_source_local());
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceType = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn roundtrip_video_source_type_gst() {
    let original = VideoSourceType::Gst(sample_video_source_gst());
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoSourceType = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_source_type_local() {
    insta::assert_json_snapshot!(VideoSourceType::Local(sample_video_source_local()));
}

#[test]
fn snapshot_video_source_type_gst() {
    insta::assert_json_snapshot!(VideoSourceType::Gst(sample_video_source_gst()));
}

#[test]
fn snapshot_video_source_type_onvif() {
    insta::assert_json_snapshot!(VideoSourceType::Onvif(sample_video_source_onvif()));
}

#[test]
fn snapshot_video_source_type_redirect() {
    insta::assert_json_snapshot!(VideoSourceType::Redirect(sample_video_source_redirect()));
}

// ===========================================================================
// stream.rs — bidirectional types (roundtrip + snapshot)
// ===========================================================================

#[test]
fn roundtrip_video_capture_configuration() {
    let original = sample_video_capture_configuration();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoCaptureConfiguration = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_capture_configuration() {
    insta::assert_json_snapshot!(sample_video_capture_configuration());
}

#[test]
fn roundtrip_capture_configuration_video() {
    let original = CaptureConfiguration::Video(sample_video_capture_configuration());
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: CaptureConfiguration = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_capture_configuration_video() {
    insta::assert_json_snapshot!(CaptureConfiguration::Video(
        sample_video_capture_configuration()
    ));
}

#[test]
#[allow(deprecated)]
fn roundtrip_capture_configuration_redirect() {
    let original = CaptureConfiguration::Redirect(super::stream::RedirectCaptureConfiguration {});
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: CaptureConfiguration = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn roundtrip_extended_configuration_default() {
    let original = ExtendedConfiguration::default();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: ExtendedConfiguration = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn roundtrip_extended_configuration_all_set() {
    let original = sample_extended_configuration();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: ExtendedConfiguration = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_extended_configuration() {
    insta::assert_json_snapshot!(sample_extended_configuration());
}

#[test]
fn deserialize_extended_configuration_from_empty_object() {
    let deserialized: ExtendedConfiguration = serde_json::from_str("{}").unwrap();
    assert_eq!(deserialized, ExtendedConfiguration::default());
}

#[test]
fn roundtrip_stream_information() {
    let original = sample_stream_information();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: StreamInformation = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_stream_information() {
    insta::assert_json_snapshot!(sample_stream_information());
}

#[test]
fn snapshot_stream_status() {
    let val = StreamStatus {
        id: Uuid::nil(),
        running: true,
        error: None,
        video_and_stream: sample_video_and_stream_information(),
        mavlink: Some(MavlinkComponent {
            system_id: 1,
            component_id: 100,
        }),
    };
    insta::assert_json_snapshot!(val);
}

#[test]
fn roundtrip_json_stream_status() {
    let val = StreamStatus {
        id: Uuid::nil(),
        running: true,
        error: None,
        video_and_stream: sample_video_and_stream_information(),
        mavlink: Some(MavlinkComponent {
            system_id: 1,
            component_id: 100,
        }),
    };
    let json = serde_json::to_string(&val).unwrap();
    let deserialized: StreamStatus = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

#[test]
fn roundtrip_mavlink_component() {
    let original = MavlinkComponent {
        system_id: 1,
        component_id: 100,
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: MavlinkComponent = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.system_id, original.system_id);
    assert_eq!(deserialized.component_id, original.component_id);
}

#[test]
fn snapshot_mavlink_component() {
    insta::assert_json_snapshot!(MavlinkComponent {
        system_id: 1,
        component_id: 100,
    });
}

#[test]
fn roundtrip_video_and_stream_information() {
    let original = sample_video_and_stream_information();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VideoAndStreamInformation = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn snapshot_video_and_stream_information() {
    insta::assert_json_snapshot!(sample_video_and_stream_information());
}

// ===========================================================================
// controls.rs — Serialize-only (snapshot tests)
// ===========================================================================

#[test]
fn snapshot_control_default() {
    insta::assert_json_snapshot!(Control::default());
}

#[test]
fn snapshot_control_with_slider() {
    let val = Control {
        name: "Brightness".into(),
        cpp_type: "int".into(),
        id: 42,
        state: ControlState {
            is_disabled: false,
            is_inactive: false,
        },
        configuration: ControlType::Slider(ControlSlider {
            default: 128,
            value: 200,
            step: 1,
            max: 255,
            min: 0,
        }),
    };
    insta::assert_json_snapshot!(val);
}

#[test]
fn snapshot_control_type_bool() {
    insta::assert_json_snapshot!(ControlType::Bool(ControlBool {
        default: 0,
        value: 1,
    }));
}

#[test]
fn snapshot_control_type_slider() {
    insta::assert_json_snapshot!(ControlType::Slider(ControlSlider {
        default: 50,
        value: 75,
        step: 5,
        max: 100,
        min: 0,
    }));
}

#[test]
fn snapshot_control_type_menu() {
    insta::assert_json_snapshot!(ControlType::Menu(ControlMenu {
        default: 0,
        value: 1,
        options: vec![
            ControlOption {
                name: "Auto".into(),
                value: 0,
            },
            ControlOption {
                name: "Manual".into(),
                value: 1,
            },
        ],
    }));
}

#[test]
fn snapshot_control_state() {
    insta::assert_json_snapshot!(ControlState::default());
}

#[test]
fn snapshot_control_bool() {
    insta::assert_json_snapshot!(ControlBool {
        default: 0,
        value: 1,
    });
}

#[test]
fn snapshot_control_slider() {
    insta::assert_json_snapshot!(ControlSlider {
        default: 50,
        value: 75,
        step: 5,
        max: 100,
        min: 0,
    });
}

#[test]
fn snapshot_control_menu() {
    insta::assert_json_snapshot!(ControlMenu {
        default: 0,
        value: 1,
        options: vec![
            ControlOption {
                name: "Auto".into(),
                value: 0,
            },
            ControlOption {
                name: "Manual".into(),
                value: 1,
            },
        ],
    });
}

#[test]
fn snapshot_control_option() {
    insta::assert_json_snapshot!(ControlOption {
        name: "Auto".into(),
        value: 0,
    });
}

// ===========================================================================
// signalling.rs — Serialize + Deserialize (snapshot + JSON roundtrip)
// ===========================================================================

#[test]
fn snapshot_message_question_peer_id() {
    insta::assert_json_snapshot!(Message::Question(Question::PeerId));
}

#[test]
fn snapshot_message_question_start_session() {
    let bind = BindOffer {
        consumer_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        producer_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
    };
    insta::assert_json_snapshot!(Message::Question(Question::StartSession(bind)));
}

#[test]
fn snapshot_message_answer_peer_id() {
    let answer = PeerIdAnswer {
        id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    };
    insta::assert_json_snapshot!(Message::Answer(Answer::PeerId(answer)));
}

#[test]
fn snapshot_message_answer_available_streams() {
    let streams = vec![Stream {
        id: Uuid::parse_str("00000000-0000-0000-0000-000000000010").unwrap(),
        name: "Main Camera".into(),
        encode: Some("H264".into()),
        height: Some(1080),
        width: Some(1920),
        interval: Some("1/30".into()),
        source: Some("/dev/video0".into()),
        created: Some("2024-01-01T00:00:00Z".into()),
    }];
    insta::assert_json_snapshot!(Message::Answer(Answer::AvailableStreams(streams)));
}

#[test]
fn snapshot_message_negotiation_media() {
    let negotiation = MediaNegotiation {
        bind: sample_bind_answer(),
        sdp: RTCSessionDescription::Offer(Sdp {
            sdp: "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n".into(),
        }),
    };
    insta::assert_json_snapshot!(Message::Negotiation(Negotiation::MediaNegotiation(
        negotiation
    )));
}

#[test]
fn snapshot_message_negotiation_ice() {
    let negotiation = IceNegotiation {
        bind: sample_bind_answer(),
        ice: RTCIceCandidateInit {
            candidate: Some("candidate:1 1 UDP 2130706431 10.0.0.1 1234 typ host".into()),
            sdp_mid: Some("0".into()),
            sdp_m_line_index: Some(0),
            username_fragment: None,
        },
    };
    insta::assert_json_snapshot!(Message::Negotiation(Negotiation::IceNegotiation(
        negotiation
    )));
}

#[test]
fn snapshot_question_peer_id() {
    insta::assert_json_snapshot!(Question::PeerId);
}

#[test]
fn snapshot_question_available_streams() {
    insta::assert_json_snapshot!(Question::AvailableStreams);
}

#[test]
fn snapshot_question_start_session() {
    let bind = BindOffer {
        consumer_id: Uuid::nil(),
        producer_id: Uuid::nil(),
    };
    insta::assert_json_snapshot!(Question::StartSession(bind));
}

#[test]
fn snapshot_answer_peer_id() {
    let answer = PeerIdAnswer { id: Uuid::nil() };
    insta::assert_json_snapshot!(Answer::PeerId(answer));
}

#[test]
fn snapshot_answer_available_streams_empty() {
    insta::assert_json_snapshot!(Answer::AvailableStreams(vec![]));
}

#[test]
fn snapshot_answer_start_session() {
    insta::assert_json_snapshot!(Answer::StartSession(sample_bind_answer()));
}

#[test]
fn snapshot_negotiation_media() {
    let val = Negotiation::MediaNegotiation(MediaNegotiation {
        bind: sample_bind_answer(),
        sdp: RTCSessionDescription::Answer(Sdp {
            sdp: "v=0\r\n".into(),
        }),
    });
    insta::assert_json_snapshot!(val);
}

#[test]
fn snapshot_negotiation_ice() {
    let val = Negotiation::IceNegotiation(IceNegotiation {
        bind: sample_bind_answer(),
        ice: RTCIceCandidateInit {
            candidate: Some("candidate:1".into()),
            sdp_mid: None,
            sdp_m_line_index: None,
            username_fragment: None,
        },
    });
    insta::assert_json_snapshot!(val);
}

#[test]
fn snapshot_sdp() {
    insta::assert_json_snapshot!(Sdp {
        sdp: "v=0\r\n".into(),
    });
}

#[test]
fn roundtrip_json_sdp() {
    let original = Sdp {
        sdp: "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n".into(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: Sdp = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

#[test]
fn roundtrip_json_message_question() {
    let original = Message::Question(Question::PeerId);
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: Message = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

#[test]
fn roundtrip_json_message_answer() {
    let answer = PeerIdAnswer { id: Uuid::nil() };
    let original = Message::Answer(Answer::PeerId(answer));
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: Message = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

#[test]
fn roundtrip_json_message_negotiation_media() {
    let original = Message::Negotiation(Negotiation::MediaNegotiation(MediaNegotiation {
        bind: sample_bind_answer(),
        sdp: RTCSessionDescription::Offer(Sdp {
            sdp: "v=0\r\n".into(),
        }),
    }));
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: Message = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

// ===========================================================================
// server.rs — mixed: Serialize-only (snapshot), Deserialize-only, and both
// ===========================================================================

// --- Serialize-only types (snapshot) ---

#[test]
fn snapshot_api_video_source() {
    let val = ApiVideoSource {
        name: "USB Camera".into(),
        source: "/dev/video0".into(),
        formats: vec![sample_format()],
        controls: vec![Control::default()],
        blocked: false,
    };
    insta::assert_json_snapshot!(val);
}

#[test]
fn snapshot_info() {
    let val = Info {
        name: "mavlink-camera-manager".into(),
        version: "0.1.0".into(),
        sha: "abc1234".into(),
        build_date: "2024-01-01".into(),
        authors: "BlueRobotics".into(),
        development: Development {
            number_of_tasks: 42,
        },
    };
    insta::assert_json_snapshot!(val);
}

#[test]
fn snapshot_development() {
    insta::assert_json_snapshot!(Development {
        number_of_tasks: 42,
    });
}

#[test]
fn snapshot_onvif_device() {
    let val = OnvifDevice {
        uuid: Uuid::nil(),
        ip: "192.168.1.100".parse().unwrap(),
        types: vec!["NetworkVideoTransmitter".into()],
        hardware: Some("HW-1234".into()),
        name: Some("ONVIF Camera".into()),
        urls: vec![Url::parse("http://192.168.1.100/onvif/device_service").unwrap()],
    };
    insta::assert_json_snapshot!(val);
}

// --- Deserialize-only types ---

#[test]
fn deserialize_remove_stream() {
    let json = r#"{"name": "test-stream"}"#;
    let val: RemoveStream = serde_json::from_str(json).unwrap();
    assert_eq!(val.name, "test-stream");
}

#[test]
fn deserialize_block_source() {
    let json = r#"{"source_string": "/dev/video0"}"#;
    let val: BlockSource = serde_json::from_str(json).unwrap();
    assert_eq!(val.source_string, "/dev/video0");
}

#[test]
fn deserialize_unblock_source() {
    let json = r#"{"source_string": "/dev/video0"}"#;
    let val: UnblockSource = serde_json::from_str(json).unwrap();
    assert_eq!(val.source_string, "/dev/video0");
}

#[test]
fn deserialize_reset_settings() {
    let json = r#"{"all": true}"#;
    let val: ResetSettings = serde_json::from_str(json).unwrap();
    assert_eq!(val.all, Some(true));
}

#[test]
fn deserialize_reset_settings_without_all() {
    let json = r#"{}"#;
    let val: ResetSettings = serde_json::from_str(json).unwrap();
    assert_eq!(val.all, None);
}

#[test]
fn deserialize_reset_camera_controls() {
    let json = r#"{"device": "/dev/video0"}"#;
    let val: ResetCameraControls = serde_json::from_str(json).unwrap();
    assert_eq!(val.device, "/dev/video0");
}

#[test]
fn deserialize_xml_file_request() {
    let json = r#"{"file": "camera_definition.xml"}"#;
    let val: XmlFileRequest = serde_json::from_str(json).unwrap();
    assert_eq!(val.file, "camera_definition.xml");
}

#[test]
fn deserialize_sdp_file_request() {
    let json = r#"{"source": "/dev/video0"}"#;
    let val: SdpFileRequest = serde_json::from_str(json).unwrap();
    assert_eq!(val.source, "/dev/video0");
}

#[test]
fn deserialize_thumbnail_file_request() {
    let json = r#"{"source": "/dev/video0", "quality": 85, "target_height": 480}"#;
    let val: ThumbnailFileRequest = serde_json::from_str(json).unwrap();
    assert_eq!(val.source, "/dev/video0");
    assert_eq!(val.quality, Some(85));
    assert_eq!(val.target_height, Some(480));
}

#[test]
fn deserialize_thumbnail_file_request_minimal() {
    let json = r#"{"source": "/dev/video0"}"#;
    let val: ThumbnailFileRequest = serde_json::from_str(json).unwrap();
    assert_eq!(val.source, "/dev/video0");
    assert_eq!(val.quality, None);
    assert_eq!(val.target_height, None);
}

#[test]
fn deserialize_authenticate_onvif_device_request() {
    let json = r#"{"device_uuid": "00000000-0000-0000-0000-000000000001", "username": "admin", "password": "secret"}"#;
    let val: AuthenticateOnvifDeviceRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        val.device_uuid,
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    );
    assert_eq!(val.username, "admin");
    assert_eq!(val.password, "secret");
}

#[test]
fn deserialize_unauthenticate_onvif_device_request() {
    let json = r#"{"device_uuid": "00000000-0000-0000-0000-000000000001"}"#;
    let val: UnauthenticateOnvifDeviceRequest = serde_json::from_str(json).unwrap();
    assert_eq!(
        val.device_uuid,
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    );
}

// --- Both Serialize + Deserialize (roundtrip via JSON string comparison) ---

#[test]
fn roundtrip_json_v4l_control() {
    let original = V4lControl {
        device: "/dev/video0".into(),
        v4l_id: 42,
        value: 128,
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: V4lControl = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

#[test]
fn snapshot_v4l_control() {
    insta::assert_json_snapshot!(V4lControl {
        device: "/dev/video0".into(),
        v4l_id: 42,
        value: 128,
    });
}

#[test]
fn roundtrip_json_post_stream() {
    let original = PostStream {
        name: "Test Stream".into(),
        source: "/dev/video0".into(),
        stream_information: sample_stream_information(),
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: PostStream = serde_json::from_str(&json).unwrap();
    let re_serialized = serde_json::to_string(&deserialized).unwrap();
    assert_eq!(json, re_serialized);
}

#[test]
fn snapshot_post_stream() {
    let val = PostStream {
        name: "Test Stream".into(),
        source: "/dev/video0".into(),
        stream_information: sample_stream_information(),
    };
    insta::assert_json_snapshot!(val);
}
