use tracing::*;

use std::{str::FromStr, sync::Arc};

use tokio::sync::RwLock;

use anyhow::Result;
use gst::prelude::*;
use url::Url;

use mavlink_camera_manager::{
    logger, settings,
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

async fn get_loopback_device() -> Result<VideoSourceType> {
    let cameras_available = VideoSourceLocal::cameras_available().await;

    let loopback_device = cameras_available
        .iter()
        .find(|&video_source| {
            if let VideoSourceType::Local(video_source_local) = video_source {
                if let VideoSourceLocalType::V4L2Loopback(_) = video_source_local.typ {
                    return true;
                }
            }

            false
        })
        .expect("No v4l2loopback device found.")
        .to_owned();

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
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)RGB\"",
                    " ! videoconvert",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)I420\"",
                    " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                    " ! h264parse",
                    " ! capsfilter caps=\"video/x-h264, profile=(string)constrained-baseline, stream-format=(string)byte-stream, alignment=(string)au\"",
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
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)RGB\"",
                    " ! videoconvert",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)I420\"",
                    " ! jpegenc quality=85 idct-method=ifast",
                    " ! jpegparse",
                    " ! capsfilter caps=\"image/jpeg, colorspace=(string)sYUV, sampling=(string)YCbCr-4:2:0, colorimetry=(string)bt601\"",
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
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)RGB\"",
                    " ! videoconvert",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)YUY2\"",
                    " ! rawvideoparse use-sink-caps=true",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, pixel-aspect-ratio=(fraction)1/1, interlace-mode=(string)progressive, format=(string)YUY2, colorimetry=(string)bt601\"",
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
        debug!("Posting EOS to V4l2Loopback Pipieline...");
        self.pipeline.post_message(gst::message::Eos::new())?;

        debug!("Waiting for V4l2Loopback Pipieline to receive EOS...");
        self.bus_task.await?;

        debug!("Setting V4l2Loopback Pipieline to Null...");
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
        timeout_secs: f32,
    ) -> Result<Self> {
        let CaptureConfiguration::Video(configuration) = &video_and_stream_information
            .stream_information
            .configuration
        else {
            unreachable!();
        };

        let endpoint = &video_and_stream_information.stream_information.endpoints[0];
        let scheme = endpoint.scheme().to_lowercase();
        let address = endpoint.host_str().unwrap();
        let port = endpoint.port().unwrap();

        let source_description = match scheme.as_str() {
            "udp" => format!("udpsrc address={address} port={port} do-timestamp=true"),
            "rtsp" => {
                format!("rtspsrc location={endpoint} is-live=true latency=0")
            }
            _ => unimplemented!(),
        };

        let pipeline_description = match configuration.encode {
            VideoEncodeType::H264 => format!(
                concat!(
                    "{source_description}",
                    " ! capsfilter caps=\"application/x-rtp, clock-rate=(int)90000, encoding-name=(string)H264, a-framerate=(string){framerate}, payload=(int)96\"",
                    " ! rtph264depay",
                    " ! h264parse",
                    " ! avdec_h264 discard-corrupted-frames=true",
                    " ! videoconvert",
                    " ! identity check-imperfect-timestamp=true check-imperfect-offset=true",
                    " ! qrtimestampsink name=qrsink sync=false",
                ),
                source_description = source_description,
                framerate = format!("{:.6}", configuration.frame_interval.denominator as f32 / configuration.frame_interval.numerator as f32),
            ),
            VideoEncodeType::Mjpg => format!(
                concat!(
                    "{source_description}",
                    " ! capsfilter caps=\"application/x-rtp, clock-rate=(int)90000, encoding-name=(string)JPEG, a-framerate=(string){framerate}, payload=(int)26\"",
                    " ! rtpjpegdepay",
                    " ! jpegparse",
                    " ! jpegdec",
                    " ! videoconvert",
                    " ! identity check-imperfect-timestamp=true check-imperfect-offset=true",
                    " ! qrtimestampsink name=qrsink sync=false",
                ),
                source_description = source_description,
                framerate = format!("{:.6}", configuration.frame_interval.denominator as f32 / configuration.frame_interval.numerator as f32),
            ),
            VideoEncodeType::Yuyv => format!(
                concat!(
                    "{source_description}",
                    " ! capsfilter caps=\"application/x-rtp, clock-rate=(int)90000, encoding-name=(string)RAW, sampling=(string)YCbCr-4:2:0, depth=(string)8, width=(string){width}, height=(string){height}, colorimetry=(string)BT601-5, payload=(int)96, a-framerate=(string){framerate}\"",
                    " ! rtpvrawdepay",
                    " ! rawvideoparse width={width} height={height} framerate={framerate_num}/{framerate_den} format=i420 colorimetry=bt601 pixel-aspect-ratio=1/1 interlaced=false",
                    " ! videoconvert",
                    " ! qrtimestampsink name=qrsink sync=false",
                ),
                source_description = source_description,
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
                framerate = format!("{:.6}", configuration.frame_interval.denominator as f32 / configuration.frame_interval.numerator as f32),
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
                debug!("All buffers received, posting EOS to QrTimestampSink Pipeline...");
                pipeline.post_message(gst::message::Eos::new()).unwrap();
            }

            None
        });

        let bus_task = new_bus_task(pipeline.downgrade());

        let pipeline_weak = pipeline.downgrade();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs_f32(timeout_secs)).await;

            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };

            error!("Timeout for QrTimestampSink Pipeline");

            pipeline.post_message(gst::message::Eos::new()).unwrap();
        });

        Ok(Self {
            pipeline,
            bus_task,
            latencies,
        })
    }

    pub async fn get_latencies(self) -> Result<Latencies> {
        get_latencies(&self.pipeline, self.bus_task, self.latencies).await
    }
}

struct Baseline {
    pipeline: gst::Pipeline,
    bus_task: tokio::task::JoinHandle<()>,
    latencies: Arc<RwLock<Vec<i64>>>,
}

impl Baseline {
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

        let pipeline_description = match configuration.encode {
            VideoEncodeType::H264 => format!(
                concat!(
                    // source
                    "qrtimestampsrc do-timestamp=true",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)RGB\"",
                    " ! videoconvert",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)I420\"",
                    " ! x264enc tune=zerolatency speed-preset=ultrafast bitrate=5000",
                    " ! h264parse",
                    " ! capsfilter caps=\"video/x-h264, profile=(string)constrained-baseline, stream-format=(string)byte-stream, alignment=(string)au\"",
                    // sink
                    " ! avdec_h264 discard-corrupted-frames=true",
                    " ! videoconvert",
                    " ! identity check-imperfect-timestamp=true check-imperfect-offset=true",
                    " ! qrtimestampsink name=qrsink sync=false",
                ),
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
            ),
            VideoEncodeType::Mjpg => format!(
                concat!(
                    // source
                    "qrtimestampsrc do-timestamp=true",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)RGB\"",
                    " ! videoconvert",
                    " ! capsfilter caps = \"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)I420\"",
                    " ! jpegenc quality=85 idct-method=ifast",
                    " ! capsfilter caps = \"image/jpeg, interlace-mode=(string)progressive, colorimetry=(string)bt601, chroma-site=(string)jpeg\"",
                    // sink
                    " ! jpegdec",
                    " ! videoconvert",
                    " ! identity check-imperfect-timestamp=true check-imperfect-offset=true",
                    " ! qrtimestampsink name=qrsink sync=false",
                ),
                width = configuration.width,
                height = configuration.height,
                framerate_num = configuration.frame_interval.denominator,
                framerate_den = configuration.frame_interval.numerator,
            ),
            VideoEncodeType::Yuyv => format!(
                concat!(
                    // source
                    "qrtimestampsrc do-timestamp=true",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)RGB\"",
                    " ! videoconvert",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, format=(string)YUY2\"",
                    " ! rawvideoparse use-sink-caps=true disable-passthrough=true",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, pixel-aspect-ratio=(fraction)1/1, interlace-mode=(string)progressive, format=(string)YUY2, colorimetry=(string)bt601\"",
                    // sink
                    " ! rawvideoparse use-sink-caps=true disable-passthrough=true",
                    " ! capsfilter caps=\"video/x-raw, width=(int){width}, height=(int){height}, framerate=(fraction){framerate_num}/{framerate_den}, pixel-aspect-ratio=(fraction)1/1, interlace-mode=(string)progressive, format=(string)YUY2, colorimetry=(string)bt601\"",
                    " ! videoconvert",
                    " ! identity check-imperfect-timestamp=true check-imperfect-offset=true",
                    " ! qrtimestampsink name=qrsink sync=false",
                ),
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
                debug!("All buffers received, posting EOS to QrTimestampSink Pipeline...");
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

    pub async fn get_latencies(self) -> Result<Latencies> {
        get_latencies(&self.pipeline, self.bus_task, self.latencies).await
    }
}

pub async fn start_pipeline(pipeline: &gst::Pipeline, wait: bool) -> Result<()> {
    pipeline.set_state(gst::State::Playing)?;

    if wait {
        while pipeline.current_state() != gst::State::Playing {
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    }

    Ok(())
}

async fn get_latencies(
    pipeline: &gst::Pipeline,
    bus_task: tokio::task::JoinHandle<()>,
    latencies: Arc<RwLock<Vec<i64>>>,
) -> Result<Latencies> {
    bus_task.await?;

    // Cleanup
    debug!("Setting qrtimestamp to Null...");
    pipeline.set_state(gst::State::Null).unwrap();

    Ok(Latencies::new(
        latencies
            .read()
            .await
            .iter()
            .cloned()
            .map(|i| i as f64)
            .collect::<Vec<f64>>(),
    ))
}

fn new_bus_task(pipeline_weak: gst::glib::WeakRef<gst::Pipeline>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };

        let Some(bus) = pipeline.bus() else {
            return;
        };

        // TODO: Move this to gst bus tokio async pattern so this tokio task can be cancellable
        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            use gst::MessageView;

            match msg.view() {
                MessageView::Eos(..) => {
                    debug!("EOS Recived");
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

        debug!("Bus task finished");
    })
}

#[derive(Debug)]
struct LatenciesStats {
    latency_min: f64,
    latency_max: f64,
    latency_mean: f64,
    latency_std_dev: f64,
    latency_variance: f64,
    jitter_min: f64,
    jitter_max: f64,
    jitter_mean: f64,
    jitter_std_dev: f64,
    jitter_variance: f64,
}

#[derive(Debug)]
struct Latencies {
    latencies: Vec<f64>,
    jitters: Vec<f64>,
    stats: LatenciesStats,
}

impl Latencies {
    pub fn new(latencies: Vec<f64>) -> Self {
        // Note: We are skipping the first frame as it will always have a high value, possibly from the negotiation
        let latencies = &latencies[1..];
        trace!("latencies: {latencies:?}");

        let jitters = &latencies
            .windows(2)
            .map(|a| a[1] - a[0])
            .collect::<Vec<_>>()[..];
        trace!("jitters: {jitters:?}");

        use statrs::statistics::Statistics;
        Self {
            latencies: latencies.to_owned(),
            jitters: jitters.to_owned(),
            stats: LatenciesStats {
                latency_min: latencies.min(),
                latency_max: latencies.max(),
                latency_mean: latencies.mean(),
                latency_std_dev: latencies.std_dev(),
                latency_variance: latencies.variance(),
                jitter_min: jitters.min(),
                jitter_max: jitters.max(),
                jitter_mean: jitters.mean(),
                jitter_std_dev: jitters.std_dev(),
                jitter_variance: jitters.variance(),
            },
        }
    }
}

async fn compute_baseline_latency(
    video_and_stream_information: &VideoAndStreamInformation,
    buffers: usize,
) -> Latencies {
    let baseline = Baseline::try_new(video_and_stream_information, buffers)
        .await
        .unwrap();
    start_pipeline(&baseline.pipeline, true).await.unwrap();
    baseline.get_latencies().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    settings::manager::init(None).await;
    stream::manager::init();
    logger::manager::init();

    gst::init().unwrap();

    let fps = 30;
    let width = 320;
    let height = width;

    let udp_port = 5600;
    let rtsp_port = 8554;
    let host = "127.0.0.1";

    let buffers = 100;

    let acceptable_latency_h264 = 10.;
    let acceptable_latency_yuyv = 30.;
    let acceptable_latency_mjpg = 10.;

    let test_cases = [
        (
            VideoEncodeType::H264,
            format!("udp://{host}:{udp_port}"),
            acceptable_latency_h264,
        ),
        (
            VideoEncodeType::Mjpg,
            format!("udp://{host}:{udp_port}"),
            acceptable_latency_mjpg,
        ),
        (
            VideoEncodeType::Yuyv,
            format!("udp://{host}:{udp_port}"),
            acceptable_latency_yuyv,
        ),
        (
            VideoEncodeType::H264,
            format!("rtsp://{host}:{rtsp_port}/test"),
            acceptable_latency_h264,
        ),
        (
            VideoEncodeType::Mjpg,
            format!("rtsp://{host}:{rtsp_port}/test"),
            acceptable_latency_mjpg,
        ),
        (
            VideoEncodeType::Yuyv,
            format!("rtsp://{host}:{rtsp_port}/test"),
            acceptable_latency_yuyv,
        ),
    ];

    for (encode, endpoint, expected_pipeline_latency) in test_cases {
        let video_and_stream_information = VideoAndStreamInformation {
            name: "QRTimeStamp - QR".to_string(),
            stream_information: StreamInformation {
                endpoints: vec![Url::from_str(endpoint.as_str()).unwrap()],
                configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                    encode,
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
            video_source: get_loopback_device().await.unwrap(),
        };
        info!("Testing for: {video_and_stream_information:#?}");

        info!("Building v4lloopback pipeline (video generation with qrtimestampsrc)...");
        let loopback = V4l2LoopBack::try_new(&video_and_stream_information).unwrap();
        start_pipeline(&loopback.pipeline, true).await.unwrap();

        info!("Building MCM stream...");
        let stream = Stream::try_new(&video_and_stream_information)
            .await
            .unwrap();
        let stream_id = stream.id().await.unwrap();
        stream::manager::Manager::add_stream(stream).await.unwrap();

        info!("Getting baseline latency...");
        let baseline_latencies =
            compute_baseline_latency(&video_and_stream_information, buffers).await;
        let baseline_latency = baseline_latencies.stats.latency_mean;
        let baseline_max_jitter = baseline_latencies.stats.jitter_max;
        info!("Baseline latency: {:#?}", &baseline_latencies.stats);

        info!("Building qrtimestamp pipeline (video receiver with qrtimestampsink)...");
        let qrtimestampsink = QrTimeStampSink::try_new(&video_and_stream_information, buffers, 60.)
            .await
            .unwrap();
        start_pipeline(&qrtimestampsink.pipeline, false)
            .await
            .unwrap();

        info!("Waiting for QrTimestampSink Pipeline to finish...");
        let latencies_result = qrtimestampsink.get_latencies().await;

        info!("Finishing loopback pipeline...");
        let loopback_result = loopback.finish().await;

        info!("Finishing MCM stream...");
        stream::manager::Manager::remove_stream(&stream_id)
            .await
            .unwrap();

        info!("Results appended...");

        info!("Evaluating results...");
        loopback_result.unwrap();
        let latencies = latencies_result.unwrap();
        info!("Pipeline latency: {:#?}", &latencies.stats);
        assert!(!latencies.latencies.is_empty());

        let latencies_compensated = latencies.stats.latency_mean - baseline_latency;
        let compensated_expected_pipeline_latency = expected_pipeline_latency + baseline_max_jitter;
        info!("Asserting {latencies_compensated} <= {compensated_expected_pipeline_latency}...");
        assert!(latencies_compensated <= compensated_expected_pipeline_latency);

        let jitter_mean = latencies.stats.jitter_mean;
        info!("Asserting {jitter_mean} <= {baseline_max_jitter}...");
        assert!(latencies.stats.jitter_mean <= baseline_max_jitter);
    }

    debug!("END");
}
