use tracing::*;

use std::{str::FromStr, sync::Arc};

use tokio::sync::RwLock;

use anyhow::*;
use gst::prelude::*;
use url::Url;

use mavlink_camera_manager::{
    settings,
    stream::{
        self,
        types::{
            CaptureConfiguration, ExtendedConfiguration, StreamInformation,
            VideoCaptureConfiguration,
        },
        Stream,
    },
    video::{
        types::{FrameInterval, VideoEncodeType, VideoSourceType},
        video_source::VideoSourceAvailable,
        video_source_local::{VideoSourceLocal, VideoSourceLocalType},
    },
    video_stream::types::VideoAndStreamInformation,
};

fn get_loopback_device() -> Result<VideoSourceType> {
    let cameras_available = VideoSourceLocal::cameras_available();
    dbg!(&cameras_available);

    let loopback_device = cameras_available
        .iter()
        .find(|&video_source| {
            dbg!(video_source.inner().source_string());
            if let VideoSourceType::Local(video_source_local) = video_source {
                if let VideoSourceLocalType::V4L2Loopback(_) = video_source_local.typ {
                    return true;
                }
            }

            false
        })
        .unwrap()
        .to_owned();
    dbg!(&loopback_device);

    Ok(loopback_device)
}

struct V4l2LoopBack {
    bus_task: tokio::task::JoinHandle<()>,
    pipeline: gst::Pipeline,
}

impl V4l2LoopBack {
    pub fn try_new(video_and_stream_information: &VideoAndStreamInformation) -> Result<Self> {
        let device_path = video_and_stream_information
            .video_source
            .inner()
            .source_string();

        let CaptureConfiguration::Video(configuration) = &video_and_stream_information
            .stream_information
            .configuration
        else {
            unreachable!();
        };

        let pipeline_description = match configuration.encode {
            VideoEncodeType::H264 => format!(
                concat!(
                    "qrtimestampsrc do-timestamp=true",
                    " ! video/x-raw,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! videoconvert",
                    " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                    " ! h264parse",
                    " ! video/x-h264,profile=constrained-baseline,stream-format=byte-stream,alignment=au",
                    " ! v4l2sink device={device_path} sync=false",
                ),
                height = configuration.height,
                width = configuration.width,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
                device_path = device_path,
            ),
            VideoEncodeType::H265 => format!(
                concat!(
                    "qrtimestampsrc do-timestamp=true",
                    " ! video/x-raw,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! videoconvert",
                    " ! x265enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                    " ! h265parse",
                    " ! video/x-h265,profile=constrained-baseline,stream-format=byte-stream,alignment=au",
                    " ! v4l2sink device={device_path} sync=false",
                ),
                height = configuration.height,
                width = configuration.width,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
                device_path = device_path,
            ),
            VideoEncodeType::Mjpg => format!(
                concat!(
                    "qrtimestampsrc do-timestamp=true",
                    " ! video/x-raw,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! videoconvert",
                    " ! jpegenc quality=85, idct-method=ifast",
                    " ! jpegparse",
                    " ! video/image/jpeg",
                    " ! v4l2sink device={device_path} sync=false",
                ),
                height = configuration.height,
                width = configuration.width,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
                device_path = device_path,
            ),
            VideoEncodeType::Rgb => format!(
                concat!(
                    "qrtimestampsrc do-timestamp=true",
                    " ! video/x-raw,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! videoconvert",
                    " ! rawvideoparse",
                    " ! video/x-raw,format=RGB",
                    " ! v4l2sink device={device_path} sync=false",
                ),
                height = configuration.height,
                width = configuration.width,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
                device_path = device_path,
            ),
            VideoEncodeType::Yuyv => format!(
                concat!(
                    "qrtimestampsrc do-timestamp=true",
                    " ! video/x-raw,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! videoconvert",
                    " ! rawvideoparse",
                    " ! video/x-raw,format=UYVY",
                    " ! v4l2sink device={device_path} sync=false",
                ),
                height = configuration.height,
                width = configuration.width,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
                device_path = device_path,
            ),
            _ => unimplemented!(),
        };
        dbg!(&pipeline_description);

        let pipeline = gst::parse::launch(&pipeline_description)?
            .downcast::<gst::Pipeline>()
            .unwrap();

        let bus_task = new_bus_task(pipeline.downgrade());

        Ok(Self { bus_task, pipeline })
    }

    pub async fn finish(self) -> Result<()> {
        println!("Posting EOS to v4l2pipeline...");
        self.pipeline.post_message(gst::message::Eos::new())?;

        println!("Waiting for v4l2pipeline to receive EOS...");
        self.bus_task.await?;

        println!("Setting v4l2pipeline to Null...");
        self.pipeline.set_state(gst::State::Null)?;
        while self.pipeline.current_state() != gst::State::Null {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        Ok(())
    }
}

struct QrTimeStampSink {
    pipeline: gst::Pipeline,
    bus_task: tokio::task::JoinHandle<()>,
    latencies: Arc<RwLock<Vec<i64>>>,
}

impl QrTimeStampSink {
    pub async fn try_new(
        video_and_stream_information: &VideoAndStreamInformation,
        buffers: usize,
    ) -> Result<Self> {
        let CaptureConfiguration::Video(configuration) = &video_and_stream_information
            .stream_information
            .configuration
        else {
            unreachable!();
        };

        let endpoint = &video_and_stream_information.stream_information.endpoints[0];
        let address = endpoint.host_str().unwrap();
        let port = endpoint.port().unwrap();

        let pipeline_description = match configuration.encode {
            VideoEncodeType::H264 => format!(
                concat!(
                    "udpsrc do-timestamp=false address={address} port={port}",
                    " ! application/x-rtp,payload=96",
                    " ! rtph264depay",
                    " ! h264parse",
                    " ! video/x-h264,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! avdec_h264",
                    " ! videoconvert",
                    " ! qrtimestampsink name=qrsink",
                ),
                address = address,
                port = port,
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
            ),
            VideoEncodeType::H265 => format!(
                concat!(
                    "udpsrc do-timestamp=false address={address} port={port}",
                    " ! application/x-rtp,payload=96",
                    " ! rtph265depay",
                    " ! h265parse",
                    " ! video/x-h265,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! avdec_h265",
                    " ! videoconvert",
                    " ! qrtimestampsink name=qrsink",
                ),
                address = address,
                port = port,
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
            ),
            VideoEncodeType::Mjpg => format!(
                concat!(
                    "udpsrc do-timestamp=false address={address} port={port}",
                    " ! application/x-rtp,payload=96",
                    " ! rtpjpegdepay",
                    " ! jpegparse",
                    " ! image/jpeg,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! jpegdec",
                    " ! videoconvert",
                    " ! qrtimestampsink name=qrsink",
                ),
                address = address,
                port = port,
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
            ),
            VideoEncodeType::Rgb | VideoEncodeType::Yuyv => format!(
                concat!(
                    "udpsrc do-timestamp=false address={address} port={port}",
                    " ! application/x-rtp,payload=96",
                    " ! rtpvrawdepay",
                    " ! rawvideoparse",
                    " ! video/x-raw,width={width},height={height},framerate={framerate_num}/{framerate_den}",
                    " ! videoconvert",
                    " ! qrtimestampsink name=qrsink",
                ),
                address = address,
                port = port,
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
            ),
            _ => unimplemented!(),
        };

        dbg!(&pipeline_description);

        let pipeline = gst::parse::launch(&pipeline_description)
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();

        let qrtimestampsink = pipeline.by_name("qrsink").unwrap();

        let latencies = Arc::new(RwLock::new(Vec::with_capacity(buffers)));

        let latencies_cloned = latencies.clone();
        let pipeline_weak = pipeline.downgrade();
        qrtimestampsink.connect("on-render", false, move |values| {
            let _element = values[0].get::<gst::Element>().expect("Invalid argument");
            let _info = values[1]
                .get::<gst_video::VideoInfo>()
                .expect("Invalid argument");
            let diff: i64 = values[2].get::<i64>().expect("Invalid argument");

            let pipeline = pipeline_weak.upgrade()?;

            let mut latencies_cloned = latencies_cloned.blocking_write();
            latencies_cloned.push(diff);
            if latencies_cloned.len() == buffers {
                eprintln!("All buffers received, posting EOS to qrtimestampsink pipeline...");
                pipeline.post_message(gst::message::Eos::new()).unwrap();
            }

            None
        });

        let bus_task = new_bus_task(pipeline.downgrade());

        Ok(Self {
            pipeline,
            bus_task,
            latencies,
        })
    }

    pub async fn get_latencies(self) -> Result<Vec<f64>> {
        self.bus_task.await?;

        // Cleanup
        println!("Setting qrtimestamp to Null...");
        self.pipeline.set_state(gst::State::Null).unwrap();
        while self.pipeline.current_state() != gst::State::Null {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let latencies = self
            .latencies
            .read()
            .await
            .iter()
            .cloned()
            .map(|i| i as f64)
            .collect::<Vec<f64>>();

        Ok(latencies)
    }
}

pub fn start_pipeline(pipeline: &gst::Pipeline, wait: bool) -> Result<()> {
    pipeline.set_state(gst::State::Playing)?;

    if wait {
        while pipeline.current_state() != gst::State::Playing {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    Ok(())
}

fn check_latency_and_jitter(latencies: &[f64], expected_latency: f64, expected_max_jitter: f64) {
    // Notes:
    //      1. We are skipping the first frame as it will always have a high value, possibly from the negotiation
    let latencies = &latencies[1..];
    dbg!(&latencies);

    let jitters = &latencies
        .windows(2)
        .map(|a| a[1] - a[0])
        .collect::<Vec<_>>()[..];
    dbg!(&jitters);

    // Asserts
    use statrs::statistics::Statistics;
    let latency_min = latencies.min();
    let latency_max = latencies.max();
    let latency_mean = latencies.mean();
    let latency_std_dev = latencies.std_dev();
    let latency_variance = latencies.variance();
    let jitter_min = jitters.min();
    let jitter_max = jitters.max();
    let jitter_mean = jitters.mean();
    let jitter_std_dev = jitters.std_dev();
    let jitter_variance = jitters.variance();

    dbg!(
        &latency_min,
        &latency_max,
        &latency_mean,
        &latency_std_dev,
        &latency_variance
    );
    dbg!(
        &jitter_min,
        &jitter_max,
        &jitter_mean,
        &jitter_std_dev,
        &jitter_variance
    );

    dbg!(&expected_latency, &expected_max_jitter);

    assert!(latency_mean >= expected_latency - expected_max_jitter);
    assert!(latency_max <= expected_latency + expected_max_jitter);
    assert!(jitter_mean <= expected_max_jitter);
    assert!(jitter_max <= expected_max_jitter);
}

fn new_bus_task(pipeline_weak: gst::glib::WeakRef<gst::Pipeline>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // FIXME: THIS TASK SHOULD BE CANCELABLE AND NONBLOCKING

        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };

        let Some(bus) = pipeline.bus() else {
            return;
        };

        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            use gst::MessageView;

            match msg.view() {
                MessageView::Eos(..) => {
                    println!("EOS Recived");
                    break;
                }
                MessageView::Error(err) => {
                    error!(
                        "Error from {:?}: {} ({:?})",
                        err.src().map(|s| s.path_string()),
                        err.error(),
                        err.debug()
                    );
                    break;
                }
                MessageView::Latency(_latency) => {
                    pipeline.recalculate_latency().unwrap();
                }
                _ => (),
            }
        }

        println!("FINISHIG BUS TASK");
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    settings::manager::init(None);
    stream::manager::init();

    gst::init().unwrap();

    let fps = 30;
    let width = 320;
    let height = width;
    let address = "127.0.0.1";
    let port = 5600;
    let buffers = 100;

    let video_and_stream_information = VideoAndStreamInformation {
        name: "QRTimeStamp - QR".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::from_str(format!("udp://{address}:{port}").as_str()).unwrap()],
            configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                encode: VideoEncodeType::H264,
                height,
                width,
                frame_interval: FrameInterval {
                    numerator: 1,
                    denominator: fps,
                },
            }),
            extended_configuration: Some(ExtendedConfiguration {
                thermal: false,
                disable_mavlink: true,
            }),
        },
        video_source: get_loopback_device().unwrap(),
    };
    dbg!(&video_and_stream_information);

    eprintln!("Building v4lloopback pipeline (video generation with qrtimestampsrc)...");
    let loopback = V4l2LoopBack::try_new(&video_and_stream_information).unwrap();
    start_pipeline(&loopback.pipeline, false).unwrap();

    eprintln!("Building qrtimestamp pipeline (video receiver with qrtimestampsink)...");
    let qrtimestamp = QrTimeStampSink::try_new(&video_and_stream_information, buffers)
        .await
        .unwrap();
    start_pipeline(&qrtimestamp.pipeline, false).unwrap();

    eprintln!("Building MCM stream...");
    let stream = Stream::try_new(&video_and_stream_information)
        .await
        .unwrap();
    let stream_id = stream.id().await.unwrap();
    stream::manager::Manager::add_stream(stream).await.unwrap();

    eprintln!("Waiting for qrtimestamp pipeline to finish...");
    let latencies_result = qrtimestamp.get_latencies().await;

    eprintln!("Finishing loopback pipeline...");
    let loopback_result = loopback.finish().await;

    eprintln!("Finishing MCM stream...");
    stream::manager::Manager::remove_stream(&stream_id)
        .await
        .unwrap();

    eprintln!("Evaluating results...");
    loopback_result.unwrap();
    let latencies = latencies_result.unwrap();
    assert!(!latencies.is_empty());

    let expected_latency = 40.0;
    let expected_max_jitter = 1.0;
    check_latency_and_jitter(&latencies, expected_latency, expected_max_jitter);

    println!("END");
}
