//! Internal benchmark harness for profiling the stats snapshot path.
//!
//! Creates synthetic `PipelineAnalysis` instances with realistic data
//! (elements, pads, records, topology, system metrics), registers them
//! in the global registry, and then calls `full_snapshot()` in a tight
//! loop with cache invalidation to produce a reproducible flamegraph
//! workload.
//!
//! Compile with `--features bench-internal`. Run with:
//! ```bash
//! cargo flamegraph --profile profiling --features bench-internal \
//!     --bin mavlink-camera-manager -o flamegraphs/iter-N.svg \
//!     -- --mavlink udpin:127.0.0.1:5777 --bench-stats-snapshot
//! ```

use std::sync::Arc;
use std::time::Instant;

use mcm_api::v1::{
    stats::StatsLevel,
    stream::{
        CaptureConfiguration, StreamInformation, StreamStatus, VideoAndStreamInformation,
        VideoCaptureConfiguration,
    },
    video::{FrameInterval, VideoEncodeType, VideoSourceGst, VideoSourceGstType, VideoSourceType},
};
use tracing::info;

use super::element_probe::ElementProbe;
use super::pipeline_analysis;
use super::pipeline_analysis::{PipelineTopology, TopologyEdge, TopologyNode};

const NUM_PIPELINES: usize = 2;
const NUM_ELEMENTS_PER_PIPELINE: usize = 10;
const RECORDS_PER_PAD: usize = 900;
const SYSTEM_SAMPLES: usize = 120;
const ITERATIONS: usize = 500;

fn bench_stream_uuid(idx: usize) -> uuid::Uuid {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("bench-{idx}").as_bytes(),
    )
}

/// Populate a single synthetic pipeline with realistic element/pad data.
fn create_synthetic_pipeline(idx: usize) -> Arc<pipeline_analysis::PipelineAnalysis> {
    let name = format!("pipeline-bench-{idx}");
    let stream_id = bench_stream_uuid(idx).to_string();
    let analysis = Arc::new(pipeline_analysis::PipelineAnalysis::new(
        name, stream_id, 33.3,
    ));

    let element_names: Vec<String> = (0..NUM_ELEMENTS_PER_PIPELINE)
        .map(|e| format!("element_{e}"))
        .collect();

    let element_types = [
        "v4l2src",
        "capsfilter",
        "videoconvert",
        "x264enc",
        "h264parse",
        "rtph264pay",
        "udpsink",
        "queue",
        "tee",
        "identity",
    ];

    // Create element probes and populate with synthetic records
    {
        let mut elements = analysis.elements_lock();
        for (e, el_name) in element_names.iter().enumerate() {
            let el_type = element_types[e % element_types.len()];
            let probe = Arc::new(ElementProbe::new(
                el_name.clone(),
                el_type.to_string(),
                StatsLevel::Full,
                RECORDS_PER_PAD,
                false,
            ));

            // Register sink and src pads
            let sink_pad = probe.register_sink_pad("sink").unwrap();
            let src_pad = probe.register_src_pad("src").unwrap();

            // Populate pads with realistic 30fps video data
            let base_wall_ns: u64 = 1_000_000_000;
            let frame_interval_ns: u64 = 33_333_333; // ~30fps
            let base_pts_ns: u64 = 0;

            for r in 0..RECORDS_PER_PAD {
                let wall = base_wall_ns + (r as u64) * frame_interval_ns + (r as u64 % 7) * 100_000; // slight jitter
                let pts = base_pts_ns + (r as u64) * frame_interval_ns;
                let is_keyframe = r % 30 == 0;
                let size = if is_keyframe {
                    50_000
                } else {
                    5_000 + (r as u32 % 3000)
                };

                // Record on sink pad (slightly earlier wall time)
                sink_pad.record(wall - 500_000, Some(pts), size, is_keyframe);
                // Record on src pad (slightly later wall time)
                src_pad.record(wall, Some(pts), size, is_keyframe);
            }

            // Set a thread ID
            probe.update_thread_id(false);

            elements.insert(el_name.clone(), probe);
        }
    }

    // Build a synthetic topology (linear chain)
    {
        let mut topo = analysis.topology_lock();
        let mut nodes = Vec::with_capacity(element_names.len());
        let mut edges = Vec::with_capacity(element_names.len() - 1);

        for (e, el_name) in element_names.iter().enumerate() {
            let el_type = element_types[e % element_types.len()];
            nodes.push(TopologyNode {
                name: el_name.clone(),
                type_name: el_type.to_string(),
            });
            if e > 0 {
                edges.push(TopologyEdge {
                    from_node: element_names[e - 1].clone(),
                    from_pad: "src".to_string(),
                    to_node: el_name.clone(),
                    to_pad: "sink".to_string(),
                    media_type: Some("video/x-h264".to_string()),
                });
            }
        }
        *topo = Some(Arc::new(PipelineTopology {
            nodes,
            edges,
            sinks: Vec::new(),
        }));
    }

    // Populate system metrics buffer
    {
        let mut sys = analysis.system.lock().unwrap();
        for s in 0..SYSTEM_SAMPLES {
            let t = s as f64 / SYSTEM_SAMPLES as f64;
            sys.record(
                25.0 + 10.0 * t,              // cpu_pct: ramps 25-35%
                1.5 + 0.5 * (t * 6.0).sin(),  // load_1m: oscillates
                45.0 + 5.0 * t,               // mem_used_pct: ramps
                55.0 + 3.0 * (t * 4.0).cos(), // temperature: oscillates
            );
        }
    }

    analysis
}

/// Run the benchmark: create synthetic data, measure full_snapshot throughput.
pub fn run_bench() {
    info!(
        "bench-stats-snapshot: creating {NUM_PIPELINES} synthetic pipelines \
         ({NUM_ELEMENTS_PER_PIPELINE} elements, {RECORDS_PER_PAD} records/pad)..."
    );

    // Create and register synthetic pipelines
    let mut handles = Vec::with_capacity(NUM_PIPELINES);
    for i in 0..NUM_PIPELINES {
        let analysis = create_synthetic_pipeline(i);
        pipeline_analysis::register(&analysis);
        handles.push(analysis); // keep Arc alive
    }

    let mock_streams_info: Vec<StreamStatus> = (0..NUM_PIPELINES)
        .map(|i| StreamStatus {
            id: bench_stream_uuid(i),
            running: true,
            error: None,
            video_and_stream: VideoAndStreamInformation {
                name: format!("bench-stream-{i}"),
                stream_information: StreamInformation {
                    endpoints: vec![],
                    configuration: CaptureConfiguration::Video(VideoCaptureConfiguration {
                        encode: VideoEncodeType::H264,
                        height: 1080,
                        width: 1920,
                        frame_interval: FrameInterval {
                            numerator: 1,
                            denominator: 30,
                        },
                    }),
                    extended_configuration: None,
                },
                video_source: VideoSourceType::Gst(VideoSourceGst {
                    name: format!("Bench source {i}"),
                    source: VideoSourceGstType::Fake("ball".to_string()),
                }),
            },
            mavlink: None,
        })
        .collect();

    let buffer_limit: usize = std::env::var("BENCH_BUFFER_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    info!("bench-stats-snapshot: running {ITERATIONS} full_snapshot({buffer_limit}) iterations...");
    let t0 = Instant::now();
    let mut json_buf: Vec<u8> = Vec::with_capacity(512 * 1024);
    for i in 0..ITERATIONS {
        pipeline_analysis::invalidate_snapshot_cache();
        let snap = pipeline_analysis::full_snapshot(buffer_limit, &mock_streams_info);
        json_buf.clear();
        serde_json::to_writer(&mut json_buf, &snap).expect("JSON serialization failed");
        std::hint::black_box(&json_buf);
        if (i + 1) % 100 == 0 {
            let elapsed = t0.elapsed();
            info!(
                "bench-stats-snapshot [buf={buffer_limit}]: {}/{ITERATIONS} done ({:.1}ms/iter avg)",
                i + 1,
                elapsed.as_secs_f64() * 1000.0 / (i + 1) as f64,
            );
        }
    }
    let elapsed = t0.elapsed();
    info!(
        "bench-stats-snapshot [buf={buffer_limit}]: finished {ITERATIONS} iterations in {:.3}s ({:.1} iter/s, {:.3}ms/iter)",
        elapsed.as_secs_f64(),
        ITERATIONS as f64 / elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / ITERATIONS as f64,
    );

    // Drop handles to unregister
    drop(handles);
}
