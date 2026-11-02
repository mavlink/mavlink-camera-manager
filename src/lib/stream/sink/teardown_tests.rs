use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use gst::prelude::*;
use serial_test::serial;
use tokio::sync::{RwLock as TokioRwLock, mpsc};
use url::Url;
use uuid::Uuid;

use super::{
    Sink, SinkInterface, image_sink::ImageSink, link_sink_to_tee, make_proxy_bridge,
    rtsp_sink::RtspSink, rtsp_sink::RtspSinkPersistent, udp_sink::UdpSink, unlink_sink_from_tee,
    webrtc_sink::WebRTCSink, zenoh_sink::ZenohSink,
};
use crate::{
    stream::{
        StreamState,
        gst::utils::{set_element_state_null, wait_for_element_state},
        lifecycle::LifecycleHandle,
        pipeline::runner::PipelineRunner,
        types::{CaptureConfiguration, StreamInformation, VideoCaptureConfiguration},
        webrtc::signalling_protocol::BindAnswer,
    },
    video::{
        types::{FrameInterval, VideoEncodeType, VideoSourceType},
        video_source_gst::{VideoSourceGst, VideoSourceGstType},
    },
    video_stream::types::VideoAndStreamInformation,
};

fn assert_null(element: &gst::Element) {
    assert_eq!(
        element.current_state(),
        gst::State::Null,
        "element {} must be Null before dispose (state={:?})",
        element.name(),
        element.current_state()
    );
}

fn wait_until_null(element: &gst::Element) {
    if element.current_state() != gst::State::Null {
        wait_for_element_state(element.downgrade(), gst::State::Null, 50, 5)
            .unwrap_or_else(|error| panic!("{} did not reach Null: {error:?}", element.name()));
    }
    assert_null(element);
}

async fn wait_until_active(element: &gst::Element) {
    for _ in 0..50 {
        match element.current_state() {
            gst::State::Playing | gst::State::Paused => return,
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!(
        "{} did not reach Paused or Playing (state={:?})",
        element.name(),
        element.current_state()
    );
}

fn pipeline_children(pipeline: &gst::Pipeline) -> Vec<gst::Element> {
    pipeline
        .iterate_elements()
        .into_iter()
        .filter_map(Result::ok)
        .collect()
}

fn added_children(before: &[gst::Element], after: &[gst::Element]) -> Vec<gst::Element> {
    after
        .iter()
        .filter(|element| before.iter().all(|known| known.name() != element.name()))
        .cloned()
        .collect()
}

fn stream_information(endpoints: &[&str], encode: VideoEncodeType) -> VideoAndStreamInformation {
    VideoAndStreamInformation {
        name: format!("teardown-{encode:?}"),
        stream_information: StreamInformation {
            endpoints: endpoints
                .iter()
                .map(|endpoint| Url::parse(endpoint).unwrap())
                .collect(),
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode,
                height: 120,
                width: 160,
                frame_interval: FrameInterval {
                    numerator: 1,
                    denominator: 30,
                },
            }),
            extended_configuration: None,
        },
        video_source: VideoSourceType::Gst(VideoSourceGst {
            name: "Fake".into(),
            source: VideoSourceGstType::Fake("ball".into()),
        }),
    }
}

struct TeeHarness {
    pipeline: gst::Pipeline,
    tee: gst::Element,
}

impl TeeHarness {
    fn raw() -> Self {
        Self::from_launch("videotestsrc is-live=true ! tee name=teardown_tee")
    }

    fn h264_elementary() -> Self {
        Self::from_launch(concat!(
            "videotestsrc is-live=true ! video/x-raw,width=160,height=120,framerate=30/1",
            " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=100 key-int-max=30",
            " ! h264parse config-interval=-1",
            " ! tee name=teardown_tee",
        ))
    }

    fn h264_rtp() -> Self {
        Self::from_launch(concat!(
            "videotestsrc is-live=true ! video/x-raw,width=160,height=120,framerate=30/1",
            " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=100 key-int-max=30",
            " ! h264parse config-interval=-1",
            " ! rtph264pay pt=96 config-interval=-1",
            " ! tee name=teardown_tee",
        ))
    }

    fn from_launch(description: &str) -> Self {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(description)
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        let tee = pipeline.by_name("teardown_tee").unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        wait_for_element_state(pipeline.downgrade(), gst::State::Playing, 50, 5).unwrap();
        Self { pipeline, tee }
    }

    fn request_src_pad(&self) -> gst::Pad {
        self.tee.request_pad_simple("src_%u").unwrap()
    }
}

impl Drop for TeeHarness {
    fn drop(&mut self) {
        // Drop cannot surface the error; the test already finished or is panicking.
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct LiveStreamStateHarness {
    state: StreamState,
    main_pipeline: gst::Pipeline,
    image_session: gst::Pipeline,
    webrtc_session: gst::Pipeline,
    webrtcbin: gst::Element,
    webrtc_id: Uuid,
}

impl LiveStreamStateHarness {
    async fn start() -> Self {
        gst::init().unwrap();
        let version = gst::version();
        eprintln!("GStreamer {}.{}.{}", version.0, version.1, version.2);

        let port = {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap().port()
        };
        let stream =
            stream_information(&[&format!("udp://127.0.0.1:{port}")], VideoEncodeType::H264);
        let pipeline_id = Arc::new(Uuid::new_v4());
        let mut state = StreamState::try_new(
            Arc::new(TokioRwLock::new(stream.clone())),
            pipeline_id,
            LifecycleHandle::lazy(),
            None,
        )
        .await
        .expect("StreamState with UDP + ImageSink");

        let webrtc_id = Uuid::new_v4();
        let (sender, _receiver) = mpsc::unbounded_channel();
        let webrtc = WebRTCSink::try_new(
            BindAnswer {
                consumer_id: Uuid::new_v4(),
                producer_id: Uuid::new_v4(),
                session_id: webrtc_id,
            },
            sender,
            &stream,
        )
        .unwrap();
        state
            .pipeline
            .as_mut()
            .expect("pipeline")
            .add_sink(Sink::WebRTC(webrtc))
            .await
            .expect("add WebRTC sink next to ImageSink");

        let pipeline_state = state
            .pipeline
            .as_ref()
            .expect("pipeline")
            .inner_state_as_ref();
        let main_pipeline = pipeline_state.pipeline.clone();
        let mut image_session = None;
        let mut webrtc_session = None;
        for sink in pipeline_state.sinks.values() {
            match sink {
                Sink::Image(_) => image_session = sink.pipeline().cloned(),
                Sink::WebRTC(_) => webrtc_session = sink.pipeline().cloned(),
                _ => {}
            }
        }
        let image_session = image_session.expect("ImageSink session pipeline");
        let webrtc_session = webrtc_session.expect("WebRTC session pipeline");
        let webrtcbin = webrtc_session
            .by_name(&format!("webrtcbin-{webrtc_id}"))
            .expect("webrtcbin");

        wait_until_active(main_pipeline.upcast_ref()).await;
        wait_until_active(image_session.upcast_ref()).await;
        wait_until_active(webrtc_session.upcast_ref()).await;

        Self {
            state,
            main_pipeline,
            image_session,
            webrtc_session,
            webrtcbin,
            webrtc_id,
        }
    }
}

#[test]
#[serial]
fn gst_bin_remove_does_not_null_the_child() {
    gst::init().unwrap();
    let pipeline = gst::parse::launch("videotestsrc is-live=true ! fakesink name=detached_sink")
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    wait_for_element_state(pipeline.downgrade(), gst::State::Playing, 50, 5).unwrap();

    let detached_sink = pipeline.by_name("detached_sink").unwrap();
    assert_eq!(detached_sink.current_state(), gst::State::Playing);

    pipeline.remove(&detached_sink).unwrap();
    assert_eq!(
        detached_sink.current_state(),
        gst::State::Playing,
        "gst_bin_remove must leave the child's state unchanged"
    );

    detached_sink.set_state(gst::State::Null).unwrap();
    wait_until_null(&detached_sink);
    pipeline.set_state(gst::State::Null).unwrap();
}

#[test]
#[serial]
fn unlink_sink_from_tee_nulls_removed_elements() {
    let harness = TeeHarness::raw();
    let tee_src_pad = harness.request_src_pad();
    let queue = gst::ElementFactory::make("queue").build().unwrap();
    let fake_sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("async", false)
        .build()
        .unwrap();
    let elements = [&queue, &fake_sink];
    link_sink_to_tee(&tee_src_pad, &harness.pipeline, &elements).unwrap();
    assert_ne!(queue.current_state(), gst::State::Null);
    assert_ne!(fake_sink.current_state(), gst::State::Null);

    unlink_sink_from_tee(&tee_src_pad, &harness.pipeline, &elements).unwrap();

    assert_null(&queue);
    assert_null(&fake_sink);
    assert!(queue.parent().is_none());
    assert!(fake_sink.parent().is_none());
}

#[test]
#[serial]
fn session_pipeline_is_nulled_before_proxysink_is_detached() {
    let harness = TeeHarness::raw();
    let tee_src_pad = harness.request_src_pad();
    let [proxysink, proxysrc] = make_proxy_bridge().unwrap();
    link_sink_to_tee(&tee_src_pad, &harness.pipeline, &[&proxysink]).unwrap();

    let session = gst::Pipeline::new();
    let fake_sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("async", false)
        .build()
        .unwrap();
    session.add_many([&proxysrc, &fake_sink]).unwrap();
    proxysrc.link(&fake_sink).unwrap();
    session.set_state(gst::State::Playing).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_ne!(proxysink.current_state(), gst::State::Null);
    assert_ne!(proxysrc.current_state(), gst::State::Null);

    session.set_state(gst::State::Null).unwrap();
    wait_until_null(session.upcast_ref());
    wait_until_null(&proxysrc);
    assert_ne!(
        proxysink.current_state(),
        gst::State::Null,
        "proxysink must still be live on the main pipeline when proxysrc reaches Null"
    );
    assert!(
        proxysink.parent().is_some(),
        "proxysink must still be parented until unlink_sink_from_tee"
    );

    unlink_sink_from_tee(&tee_src_pad, &harness.pipeline, &[&proxysink]).unwrap();
    assert_null(&proxysink);
    assert!(proxysink.parent().is_none());
}

fn assert_completes_within(limit: Duration, description: &str, body: impl FnOnce()) {
    let start = std::time::Instant::now();
    body();
    let elapsed = start.elapsed();
    assert!(
        elapsed < limit,
        "{description} took {elapsed:?}, expected under {limit:?}"
    );
}

fn assert_second_shutdown_is_immediate(sink: &impl SinkInterface) {
    assert_completes_within(Duration::from_millis(80), "second shutdown_session", || {
        sink.shutdown_session()
    });
}

fn pending_bus_messages(pipeline: &gst::Pipeline) -> usize {
    let bus = pipeline.bus().expect("pipeline bus");
    let mut count = 0;
    while bus.pop().is_some() {
        count += 1;
    }
    count
}

fn count_eos_posted_on(pipeline: &gst::Pipeline, body: impl FnOnce()) -> usize {
    let bus = pipeline.bus().expect("pipeline bus");
    let eos_count = Arc::new(AtomicUsize::new(0));
    bus.set_sync_handler({
        let eos_count = eos_count.clone();
        move |_, message| {
            if matches!(message.view(), gst::MessageView::Eos(_)) {
                eos_count.fetch_add(1, Ordering::SeqCst);
            }
            gst::BusSyncReply::Drop
        }
    });
    body();
    eos_count.load(Ordering::SeqCst)
}

fn webrtcbin_request_sink_pads(webrtcbin: &gst::Element) -> Vec<gst::Pad> {
    webrtcbin
        .iterate_sink_pads()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|pad| pad.name().starts_with("sink_"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn udp_unlink_nulls_session_and_proxysink() {
    gst::init().unwrap();
    let port = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().port()
    };
    let stream = stream_information(&[&format!("udp://127.0.0.1:{port}")], VideoEncodeType::H264);
    let mut sink = UdpSink::try_new(Arc::new(Uuid::new_v4()), &stream).unwrap();
    let harness = TeeHarness::h264_rtp();
    let before = pipeline_children(&harness.pipeline);
    let tee_src_pad = harness.request_src_pad();
    sink.link(&harness.pipeline, &Arc::new(Uuid::new_v4()), tee_src_pad)
        .unwrap();
    let tee_elements = added_children(&before, &pipeline_children(&harness.pipeline));
    assert!(!tee_elements.is_empty());
    sink.start().unwrap();
    wait_until_active(sink.pipeline().unwrap().upcast_ref()).await;

    let session = sink.pipeline().unwrap().clone();
    sink.unlink(&harness.pipeline, &Arc::new(Uuid::new_v4()))
        .unwrap();

    assert_null(session.upcast_ref());
    for element in &tee_elements {
        assert_null(element);
        assert!(element.parent().is_none());
    }
    assert_second_shutdown_is_immediate(&sink);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn udp_drop_without_link_nulls_session() {
    gst::init().unwrap();
    let port = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().port()
    };
    let stream = stream_information(&[&format!("udp://127.0.0.1:{port}")], VideoEncodeType::H264);
    let sink = UdpSink::try_new(Arc::new(Uuid::new_v4()), &stream).unwrap();
    let session = sink.pipeline().unwrap().clone();
    drop(sink);
    wait_until_null(session.upcast_ref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn image_unlink_nulls_tee_branch_and_releases_last_sample() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::Rgb);
    let sink_id = Arc::new(Uuid::new_v4());
    let mut sink = ImageSink::try_new(sink_id.clone(), &stream).unwrap();
    let harness = TeeHarness::raw();
    let before = pipeline_children(&harness.pipeline);
    let tee_src_pad = harness.request_src_pad();
    sink.link(&harness.pipeline, &Arc::new(Uuid::new_v4()), tee_src_pad)
        .unwrap();
    let tee_elements = added_children(&before, &pipeline_children(&harness.pipeline));
    sink.start().unwrap();
    wait_until_active(sink.pipeline().unwrap().upcast_ref()).await;

    let session = sink.pipeline().unwrap().clone();
    let appsink = session
        .by_name(&format!("AppSink-{sink_id}"))
        .expect("ImageSink appsink");
    assert!(appsink.property::<bool>("enable-last-sample"));

    sink.unlink(&harness.pipeline, &Arc::new(Uuid::new_v4()))
        .unwrap();

    assert_null(session.upcast_ref());
    assert!(!appsink.property::<bool>("enable-last-sample"));
    for element in &tee_elements {
        assert_null(element);
        assert!(element.parent().is_none());
    }
    assert_second_shutdown_is_immediate(&sink);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn image_drop_without_link_nulls_session_and_releases_last_sample() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::Rgb);
    let sink_id = Arc::new(Uuid::new_v4());
    let sink = ImageSink::try_new(sink_id.clone(), &stream).unwrap();
    let session = sink.pipeline().unwrap().clone();
    let appsink = session
        .by_name(&format!("AppSink-{sink_id}"))
        .expect("ImageSink appsink");
    drop(sink);
    wait_until_null(session.upcast_ref());
    assert!(!appsink.property::<bool>("enable-last-sample"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn webrtc_unlink_nulls_session_webrtcbin_and_proxysink() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let session_id = Uuid::new_v4();
    let (sender, _receiver) = mpsc::unbounded_channel();
    let bind = BindAnswer {
        consumer_id: Uuid::new_v4(),
        producer_id: Uuid::new_v4(),
        session_id,
    };
    let mut sink = WebRTCSink::try_new(bind, sender, &stream).unwrap();
    let harness = TeeHarness::h264_rtp();
    let before = pipeline_children(&harness.pipeline);
    let tee_src_pad = harness.request_src_pad();
    sink.link(&harness.pipeline, &Arc::new(Uuid::new_v4()), tee_src_pad)
        .unwrap();
    let tee_elements = added_children(&before, &pipeline_children(&harness.pipeline));
    sink.start().unwrap();
    wait_until_active(sink.pipeline().unwrap().upcast_ref()).await;

    let session = sink.pipeline().unwrap().clone();
    let webrtcbin = session
        .by_name(&format!("webrtcbin-{session_id}"))
        .expect("webrtcbin");

    sink.unlink(&harness.pipeline, &Arc::new(Uuid::new_v4()))
        .unwrap();

    assert_null(session.upcast_ref());
    assert_null(&webrtcbin);
    assert!(webrtcbin.parent().is_none());
    for element in &tee_elements {
        assert_null(element);
        assert!(element.parent().is_none());
    }
    assert_second_shutdown_is_immediate(&sink);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn webrtc_drop_without_link_nulls_session_and_unparents_webrtcbin() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let session_id = Uuid::new_v4();
    let (sender, _receiver) = mpsc::unbounded_channel();
    let bind = BindAnswer {
        consumer_id: Uuid::new_v4(),
        producer_id: Uuid::new_v4(),
        session_id,
    };
    let sink = WebRTCSink::try_new(bind, sender, &stream).unwrap();
    let session = sink.pipeline().unwrap().clone();
    let webrtcbin = session
        .by_name(&format!("webrtcbin-{session_id}"))
        .expect("webrtcbin");
    drop(sink);
    wait_until_null(session.upcast_ref());
    wait_until_null(&webrtcbin);
    assert!(webrtcbin.parent().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn rtsp_shutdown_does_not_mute_the_live_valve() {
    gst::init().unwrap();
    let sink_id = Arc::new(Uuid::new_v4());
    let addresses = vec![Url::parse("rtsp://0.0.0.0:8554/teardown-valve").unwrap()];
    let old_sink =
        RtspSink::try_new(sink_id, addresses.clone(), LifecycleHandle::lazy(), None).unwrap();
    let old_valve = old_sink.flow_handle().valve();

    let new_sink = RtspSink::try_new(
        Arc::new(Uuid::new_v4()),
        addresses,
        LifecycleHandle::lazy(),
        Some(RtspSinkPersistent {
            appsrc: Some(old_sink.rtsp_appsrc()),
            pts_offset: Some(old_sink.pts_offset()),
            flow_handle: Some(old_sink.flow_handle()),
        }),
    )
    .unwrap();
    let live_valve = new_sink.flow_handle().valve();
    assert_ne!(old_valve.name(), live_valve.name());
    live_valve.set_property("drop", false);

    old_sink.shutdown_session();

    assert!(old_valve.property::<bool>("drop"));
    assert!(
        !live_valve.property::<bool>("drop"),
        "old RtspSink shutdown must not close the valve that now belongs to the live pipeline"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn rtsp_unlink_after_valve_swap_removes_the_linked_valve() {
    gst::init().unwrap();
    let addresses = vec![Url::parse("rtsp://0.0.0.0:8554/teardown-unlink").unwrap()];
    let mut old_sink = RtspSink::try_new(
        Arc::new(Uuid::new_v4()),
        addresses.clone(),
        LifecycleHandle::lazy(),
        None,
    )
    .unwrap();
    let old_valve = old_sink.flow_handle().valve();
    let harness = TeeHarness::raw();
    let tee_src_pad = harness.request_src_pad();
    old_sink
        .link(&harness.pipeline, &Arc::new(Uuid::new_v4()), tee_src_pad)
        .unwrap();
    assert_ne!(old_valve.current_state(), gst::State::Null);

    let new_sink = RtspSink::try_new(
        Arc::new(Uuid::new_v4()),
        addresses,
        LifecycleHandle::lazy(),
        Some(RtspSinkPersistent {
            appsrc: Some(old_sink.rtsp_appsrc()),
            pts_offset: Some(old_sink.pts_offset()),
            flow_handle: Some(old_sink.flow_handle()),
        }),
    )
    .unwrap();
    let live_valve = new_sink.flow_handle().valve();
    live_valve.set_property("drop", false);

    old_sink
        .unlink(&harness.pipeline, &Arc::new(Uuid::new_v4()))
        .unwrap();

    assert_null(&old_valve);
    assert!(old_valve.parent().is_none());
    assert!(
        !live_valve.property::<bool>("drop"),
        "unlink of the old RTSP sink must not mute the live valve"
    );
    assert!(live_valve.parent().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn zenoh_unlink_nulls_session_and_proxysink() {
    gst::init().unwrap();
    crate::zenoh::init_for_tests().await.unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let mut sink = ZenohSink::try_new(Arc::new(Uuid::new_v4()), &stream)
        .await
        .unwrap();
    let harness = TeeHarness::h264_elementary();
    let before = pipeline_children(&harness.pipeline);
    let tee_src_pad = harness.request_src_pad();
    sink.link(&harness.pipeline, &Arc::new(Uuid::new_v4()), tee_src_pad)
        .unwrap();
    let tee_elements = added_children(&before, &pipeline_children(&harness.pipeline));
    sink.start().unwrap();
    wait_until_active(sink.pipeline().unwrap().upcast_ref()).await;

    let session = sink.pipeline().unwrap().clone();
    sink.unlink(&harness.pipeline, &Arc::new(Uuid::new_v4()))
        .unwrap();

    assert_null(session.upcast_ref());
    for element in &tee_elements {
        assert_null(element);
        assert!(element.parent().is_none());
    }
    assert_second_shutdown_is_immediate(&sink);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn zenoh_drop_without_link_nulls_session() {
    gst::init().unwrap();
    crate::zenoh::init_for_tests().await.unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let sink = ZenohSink::try_new(Arc::new(Uuid::new_v4()), &stream)
        .await
        .unwrap();
    let session = sink.pipeline().unwrap().clone();
    drop(sink);
    wait_until_null(session.upcast_ref());
}

#[test]
#[serial]
fn wait_for_element_state_returns_immediately_when_already_at_target() {
    gst::init().unwrap();
    let pipeline = gst::Pipeline::new();
    assert_eq!(pipeline.current_state(), gst::State::Null);
    assert_completes_within(
        Duration::from_millis(80),
        "wait_for_element_state on an already-Null element",
        || {
            wait_for_element_state(pipeline.downgrade(), gst::State::Null, 100, 5).unwrap();
        },
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn first_shutdown_session_skips_wait_when_already_null() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::Rgb);
    let sink = ImageSink::try_new(Arc::new(Uuid::new_v4()), &stream).unwrap();
    assert_eq!(sink.pipeline().unwrap().current_state(), gst::State::Null);
    assert_completes_within(
        Duration::from_millis(80),
        "first shutdown_session on an already-Null ImageSink",
        || sink.shutdown_session(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn pipeline_runner_stop_before_start_drops_teardown_bus_messages() {
    gst::init().unwrap();
    let pipeline = gst::Pipeline::new();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let runner =
        PipelineRunner::try_new(&pipeline, &Arc::new(Uuid::new_v4()), true, &stream).unwrap();
    runner.stop();
    pipeline
        .bus()
        .unwrap()
        .post(
            gst::message::Application::builder(gst::Structure::builder("teardown-probe").build())
                .build(),
        )
        .unwrap();
    assert_eq!(
        pending_bus_messages(&pipeline),
        0,
        "stop() must install a drop handler even if the runner task has not polled yet"
    );
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    pipeline
        .bus()
        .unwrap()
        .post(
            gst::message::Application::builder(
                gst::Structure::builder("teardown-probe-late").build(),
            )
            .build(),
        )
        .unwrap();
    assert_eq!(
        pending_bus_messages(&pipeline),
        0,
        "a late runner poll must not replace the drop handler with an unhandled bus"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn pipeline_runner_stop_after_playing_drops_teardown_bus_messages() {
    gst::init().unwrap();
    let pipeline = gst::parse::launch("videotestsrc is-live=true ! fakesink sync=false")
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let runner =
        PipelineRunner::try_new(&pipeline, &Arc::new(Uuid::new_v4()), true, &stream).unwrap();
    runner.start().unwrap();
    wait_until_active(pipeline.upcast_ref()).await;
    runner.stop();
    pipeline
        .bus()
        .unwrap()
        .post(
            gst::message::Application::builder(gst::Structure::builder("teardown-probe").build())
                .build(),
        )
        .unwrap();
    assert_eq!(
        pending_bus_messages(&pipeline),
        0,
        "teardown bus messages must be dropped, not queued after unset_sync_handler"
    );
    pipeline.set_state(gst::State::Null).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn rtsp_drop_without_unlink_closes_the_linked_valve() {
    gst::init().unwrap();
    let addresses = vec![Url::parse("rtsp://0.0.0.0:8554/teardown-drop").unwrap()];
    let sink = RtspSink::try_new(
        Arc::new(Uuid::new_v4()),
        addresses,
        LifecycleHandle::lazy(),
        None,
    )
    .unwrap();
    let valve = sink.flow_handle().valve();
    valve.set_property("drop", false);
    drop(sink);
    assert!(
        valve.property::<bool>("drop"),
        "RtspSink Drop must shutdown_session so an unlinked valve is closed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn webrtc_shutdown_session_releases_request_pad_before_null() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let session_id = Uuid::new_v4();
    let (sender, _receiver) = mpsc::unbounded_channel();
    let bind = BindAnswer {
        consumer_id: Uuid::new_v4(),
        producer_id: Uuid::new_v4(),
        session_id,
    };
    let sink = WebRTCSink::try_new(bind, sender, &stream).unwrap();
    let session = sink.pipeline().unwrap().clone();
    let webrtcbin = session
        .by_name(&format!("webrtcbin-{session_id}"))
        .expect("webrtcbin");
    assert!(
        !webrtcbin_request_sink_pads(&webrtcbin).is_empty(),
        "webrtcbin must still hold its request sink pad before shutdown"
    );

    sink.shutdown_session();

    assert!(
        webrtcbin_request_sink_pads(&webrtcbin).is_empty(),
        "shutdown_session must release the webrtcbin request pad before Nulling it"
    );
    wait_until_null(&webrtcbin);
}

#[test]
#[serial]
fn unlink_sink_from_tee_nulls_elements_when_remove_many_fails() {
    let harness = TeeHarness::raw();
    let tee_src_pad = harness.request_src_pad();
    let queue = gst::ElementFactory::make("queue").build().unwrap();
    let fake_sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("async", false)
        .build()
        .unwrap();
    let stranger = gst::ElementFactory::make("identity").build().unwrap();
    link_sink_to_tee(&tee_src_pad, &harness.pipeline, &[&queue, &fake_sink]).unwrap();
    assert_ne!(queue.current_state(), gst::State::Null);
    assert_ne!(fake_sink.current_state(), gst::State::Null);

    let result = unlink_sink_from_tee(
        &tee_src_pad,
        &harness.pipeline,
        &[&queue, &fake_sink, &stranger],
    );

    assert!(
        result.is_err(),
        "remove_many must fail when an element is not a child of the pipeline"
    );
    assert_null(&queue);
    assert_null(&fake_sink);
}

#[test]
#[serial]
fn link_sink_to_tee_cleanup_nulls_elements_after_failed_link() {
    let harness = TeeHarness::raw();
    let tee_src_pad = harness.request_src_pad();
    let queue = gst::ElementFactory::make("queue").build().unwrap();
    let videotestsrc = gst::ElementFactory::make("videotestsrc")
        .property("is-live", true)
        .build()
        .unwrap();

    let result = link_sink_to_tee(&tee_src_pad, &harness.pipeline, &[&queue, &videotestsrc]);

    assert!(result.is_err(), "queue cannot link to videotestsrc");
    assert!(queue.parent().is_none());
    assert!(videotestsrc.parent().is_none());
    assert_null(&queue);
    assert_null(&videotestsrc);
}

#[test]
#[serial]
fn set_element_state_null_on_playing_element_stays_within_lock_budget() {
    gst::init().unwrap();
    let pipeline = gst::parse::launch(
        "videotestsrc is-live=true ! fakesink name=budget_sink sync=false async=false",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    wait_for_element_state(pipeline.downgrade(), gst::State::Playing, 50, 5).unwrap();
    let fake_sink = pipeline.by_name("budget_sink").unwrap();
    assert_ne!(fake_sink.current_state(), gst::State::Null);

    assert_completes_within(
        Duration::from_millis(80),
        "set_element_state_null on a PLAYING fakesink",
        || set_element_state_null(&fake_sink),
    );
    assert_null(&fake_sink);
    pipeline.set_state(gst::State::Null).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn webrtc_shutdown_session_stops_runner_before_releasing_request_pad() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let session_id = Uuid::new_v4();
    let (sender, _receiver) = mpsc::unbounded_channel();
    let bind = BindAnswer {
        consumer_id: Uuid::new_v4(),
        producer_id: Uuid::new_v4(),
        session_id,
    };
    let sink = WebRTCSink::try_new(bind, sender, &stream).unwrap();
    let session = sink.pipeline().unwrap().clone();
    let webrtcbin = session
        .by_name(&format!("webrtcbin-{session_id}"))
        .expect("webrtcbin");
    let request_pad = webrtcbin_request_sink_pads(&webrtcbin)
        .into_iter()
        .next()
        .expect("webrtcbin request sink pad");
    let stop_flag = sink.runner_stop_flag();
    let unlinked = Arc::new(AtomicBool::new(false));
    let stopped_at_unlink = Arc::new(AtomicBool::new(false));
    request_pad.connect_unlinked({
        let unlinked = unlinked.clone();
        let stopped_at_unlink = stopped_at_unlink.clone();
        move |_pad, _peer| {
            unlinked.store(true, Ordering::SeqCst);
            stopped_at_unlink.store(stop_flag.load(Ordering::Acquire), Ordering::SeqCst);
        }
    });

    sink.shutdown_session();

    assert!(
        unlinked.load(Ordering::SeqCst),
        "shutdown_session must release the webrtcbin request pad"
    );
    assert!(
        stopped_at_unlink.load(Ordering::SeqCst),
        "pipeline_runner.stop() must run before release_request_pad so teardown errors are dropped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn udp_eos_does_not_post_teardown_eos() {
    gst::init().unwrap();
    let port = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().port()
    };
    let stream = stream_information(&[&format!("udp://127.0.0.1:{port}")], VideoEncodeType::H264);
    let sink = UdpSink::try_new(Arc::new(Uuid::new_v4()), &stream).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let session = sink.pipeline().unwrap().clone();
    let posted = count_eos_posted_on(&session, || sink.eos());
    assert_eq!(
        posted, 0,
        "eos() must not post teardown EOS; shutdown_session Nulls the session instead"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn image_eos_does_not_post_teardown_eos() {
    gst::init().unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::Rgb);
    let sink = ImageSink::try_new(Arc::new(Uuid::new_v4()), &stream).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let session = sink.pipeline().unwrap().clone();
    let posted = count_eos_posted_on(&session, || sink.eos());
    assert_eq!(
        posted, 0,
        "eos() must not post teardown EOS; shutdown_session Nulls the session instead"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn zenoh_eos_does_not_post_teardown_eos() {
    gst::init().unwrap();
    crate::zenoh::init_for_tests().await.unwrap();
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let sink = ZenohSink::try_new(Arc::new(Uuid::new_v4()), &stream)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let session = sink.pipeline().unwrap().clone();
    let posted = count_eos_posted_on(&session, || sink.eos());
    assert_eq!(
        posted, 0,
        "eos() must not post teardown EOS; shutdown_session Nulls the session instead"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn pipeline_runner_stop_drains_already_queued_bus_messages() {
    gst::init().unwrap();
    let pipeline = gst::Pipeline::new();
    pipeline
        .bus()
        .unwrap()
        .post(
            gst::message::Application::builder(
                gst::Structure::builder("queued-before-stop").build(),
            )
            .build(),
        )
        .unwrap();
    assert!(
        pipeline.bus().unwrap().have_pending(),
        "the test must queue a message before stop()"
    );
    let stream = stream_information(&["udp://127.0.0.1:9"], VideoEncodeType::H264);
    let runner =
        PipelineRunner::try_new(&pipeline, &Arc::new(Uuid::new_v4()), true, &stream).unwrap();
    runner.stop();
    assert_eq!(
        pending_bus_messages(&pipeline),
        0,
        "stop() must drain messages queued before the drop handler was installed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn webrtc_unlink_while_image_sink_is_live_nulls_only_webrtc() {
    let mut harness = LiveStreamStateHarness::start().await;
    tokio::runtime::Handle::try_current().expect("must run on a tokio worker");

    harness
        .state
        .pipeline
        .as_mut()
        .expect("pipeline")
        .remove_sink(&harness.webrtc_id)
        .await
        .expect("remove WebRTC while ImageSink stays attached");

    assert_null(harness.webrtc_session.upcast_ref());
    assert_null(&harness.webrtcbin);
    assert!(harness.webrtcbin.parent().is_none());
    assert_ne!(
        harness.image_session.current_state(),
        gst::State::Null,
        "ImageSink must stay live across WebRTC unlink (thumbnail cooldown)"
    );
    assert_ne!(
        harness.main_pipeline.current_state(),
        gst::State::Null,
        "main pipeline must stay live across WebRTC unlink"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stream_state_drop_of_live_pipeline_on_tokio_worker_under_write_lock() {
    let harness = LiveStreamStateHarness::start().await;
    tokio::runtime::Handle::try_current().expect("must run on a tokio worker");

    let write_lock = TokioRwLock::new(());
    {
        let _write_guard = write_lock.write().await;
        assert_completes_within(
            Duration::from_secs(2),
            "StreamState::drop of a live mixed pipeline under a tokio write lock",
            || drop(harness.state),
        );
    }

    wait_until_null(harness.main_pipeline.upcast_ref());
    wait_until_null(harness.image_session.upcast_ref());
    wait_until_null(harness.webrtc_session.upcast_ref());
    wait_until_null(&harness.webrtcbin);
}
