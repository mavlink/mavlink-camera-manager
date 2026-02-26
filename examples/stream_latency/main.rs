mod protocol;
mod rtsp_client;
mod udp_client;
mod webrtc_client;

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hasher},
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};
use gst::prelude::*;
use tokio::sync::mpsc;
use uuid::Uuid;

// ── Shared types ────────────────────────────────────────────────────────────

pub struct FrameSample {
    pub content_hash: u64,
    pub relative_pts_ms: i64,
    pub arrival: Instant,
    pub buffer_size: usize,
}

pub type SampleSender = mpsc::UnboundedSender<FrameSample>;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Codec {
    H264,
    H265,
}

/// Attach a pad probe that hashes each buffer's content and records the hash
/// together with (relative_pts_ms, wall-clock Instant).  Matching by content
/// hash guarantees we compare the exact same frame across clients, regardless
/// of any PTS re-stamping along the way.
pub fn attach_frame_probe(pad: &gst::Pad, client_name: String, sender: SampleSender) {
    let first_pts: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));

    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        let Some(gst::PadProbeData::Buffer(ref buffer)) = info.data else {
            return gst::PadProbeReturn::Ok;
        };

        let arrival = Instant::now();

        let Ok(map) = buffer.map_readable() else {
            return gst::PadProbeReturn::Ok;
        };
        let buffer_size = map.len();
        let mut hasher = DefaultHasher::new();
        hasher.write(map.as_slice());
        let content_hash = hasher.finish();

        let relative_pts_ms = buffer.pts().map_or(-1, |pts| {
            let pts_ns = pts.nseconds();
            let mut first = first_pts.lock().unwrap();
            let base = *first.get_or_insert(pts_ns);
            ((pts_ns - base) / 1_000_000) as i64
        });

        if sender
            .send(FrameSample {
                content_hash,
                relative_pts_ms,
                arrival,
                buffer_size,
            })
            .is_err()
        {
            eprintln!("[{client_name}] Sample channel closed");
        }

        gst::PadProbeReturn::Ok
    });
}

// ── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "stream_latency",
    about = "Measure pairwise latency between RTSP / WebRTC / UDP stream transports"
)]
struct Args {
    /// RTSP URL(s) to receive from (repeatable)
    #[arg(long = "rtsp", value_name = "URL")]
    rtsp_urls: Vec<String>,

    /// WebRTC signalling server WebSocket URL
    #[arg(long = "webrtc", value_name = "WS_URL")]
    webrtc_url: Option<String>,

    /// WebRTC producer/stream UUID (auto-detected when only one stream exists)
    #[arg(long = "producer-id", value_name = "UUID")]
    producer_id: Option<Uuid>,

    /// UDP endpoint(s) to listen on as ADDR:PORT (repeatable)
    #[arg(long = "udp", value_name = "ADDR:PORT")]
    udp_endpoints: Vec<String>,

    /// Codec hint for UDP streams
    #[arg(long, default_value = "h264")]
    codec: Codec,

    /// Measurement duration in seconds
    #[arg(long, default_value = "30")]
    duration: u64,

    /// Periodic report interval in seconds
    #[arg(long, default_value = "2")]
    report_interval: u64,

    /// Warmup period in seconds (discard initial samples)
    #[arg(long, default_value = "2")]
    warmup: u64,

    /// Optional CSV output path for raw per-frame data
    #[arg(long)]
    csv: Option<String>,
}

// ── Correlator / Reporter ───────────────────────────────────────────────────

struct ClientData {
    name: String,
    receiver: mpsc::UnboundedReceiver<FrameSample>,
    /// content_hash → (relative_pts_ms, arrival wall-clock, buffer_size)
    samples: HashMap<u64, (i64, Instant, usize)>,
    /// Arrival-ordered list of (arrival, buffer_size) for bitrate/jitter stats
    arrivals: Vec<(Instant, usize)>,
}

fn drain_samples(clients: &mut [ClientData]) {
    for client in clients.iter_mut() {
        while let Ok(sample) = client.receiver.try_recv() {
            client.samples.insert(
                sample.content_hash,
                (sample.relative_pts_ms, sample.arrival, sample.buffer_size),
            );
            client.arrivals.push((sample.arrival, sample.buffer_size));
        }
    }
}

/// For each frame in `a` that also appears in `b` (matched by content hash),
/// compute the signed arrival-time delta (positive = b arrived later).
/// Returns (sorted_deltas, matched_count).
fn compute_deltas(
    a: &HashMap<u64, (i64, Instant, usize)>,
    b: &HashMap<u64, (i64, Instant, usize)>,
) -> Vec<i64> {
    let mut deltas = Vec::new();

    for (hash, &(_, a_arrival, _)) in a {
        if let Some(&(_, b_arrival, _)) = b.get(hash) {
            let delta_us = if b_arrival >= a_arrival {
                b_arrival.duration_since(a_arrival).as_micros() as i64
            } else {
                -(a_arrival.duration_since(b_arrival).as_micros() as i64)
            };
            deltas.push(delta_us);
        }
    }

    deltas.sort();
    deltas
}

fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn format_us(us: i64) -> String {
    if us.unsigned_abs() >= 1_000_000 {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    } else if us.unsigned_abs() >= 1_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{us}us")
    }
}

fn print_client_stats(client: &ClientData) {
    let n = client.arrivals.len();
    if n < 2 {
        println!("  {}: {} frames (insufficient data)", client.name, n);
        return;
    }

    let first = client.arrivals.first().unwrap().0;
    let last = client.arrivals.last().unwrap().0;
    let wall_secs = last.duration_since(first).as_secs_f64();

    let total_bytes: usize = client.arrivals.iter().map(|&(_, sz)| sz).sum();
    let bitrate_mbps = if wall_secs > 0.0 {
        (total_bytes as f64 * 8.0) / (wall_secs * 1_000_000.0)
    } else {
        0.0
    };
    let fps = if wall_secs > 0.0 {
        (n - 1) as f64 / wall_secs
    } else {
        0.0
    };

    let mut inter_arrival_us: Vec<i64> = client
        .arrivals
        .windows(2)
        .map(|w| w[1].0.duration_since(w[0].0).as_micros() as i64)
        .collect();
    inter_arrival_us.sort();

    let mean_ia = inter_arrival_us.iter().sum::<i64>() as f64 / inter_arrival_us.len() as f64;

    // Jitter = stddev of inter-arrival times
    let variance = inter_arrival_us
        .iter()
        .map(|&d| {
            let diff = d as f64 - mean_ia;
            diff * diff
        })
        .sum::<f64>()
        / inter_arrival_us.len() as f64;
    let jitter_us = variance.sqrt();

    let ia_p50 = percentile(&inter_arrival_us, 0.50);
    let ia_p95 = percentile(&inter_arrival_us, 0.95);
    let ia_p99 = percentile(&inter_arrival_us, 0.99);
    let ia_max = *inter_arrival_us.last().unwrap();

    let avg_frame_kb = total_bytes as f64 / n as f64 / 1024.0;

    println!(
        "  {}: {n} frames, {fps:.1} fps, {bitrate_mbps:.2} Mbps, {avg_frame_kb:.1} KB/frame, jitter(stddev)={}, inter-arrival p50={} p95={} p99={} max={}",
        client.name,
        format_us(jitter_us as i64),
        format_us(ia_p50),
        format_us(ia_p95),
        format_us(ia_p99),
        format_us(ia_max),
    );
}

fn print_report(clients: &[ClientData], label: &str) {
    let total_samples: usize = clients.iter().map(|c| c.samples.len()).sum();

    println!("\n=== {label} ({total_samples} total samples, matched by content hash) ===");

    println!("--- Per-client stats ---");
    for c in clients {
        print_client_stats(c);
    }

    if clients.len() < 2 {
        return;
    }

    println!("--- Pairwise latency ---");
    for i in 0..clients.len() {
        for j in (i + 1)..clients.len() {
            let deltas = compute_deltas(&clients[i].samples, &clients[j].samples);

            let common = deltas.len();
            let a_total = clients[i].samples.len();
            let b_total = clients[j].samples.len();
            let match_pct = if a_total > 0 {
                100.0 * common as f64 / a_total as f64
            } else {
                0.0
            };

            if deltas.is_empty() {
                println!(
                    "  {} -> {}: no matched frames ({a_total} vs {b_total} samples)",
                    clients[i].name, clients[j].name
                );
                continue;
            }

            let n = deltas.len();
            let mean = deltas.iter().sum::<i64>() / n as i64;
            let min = deltas[0];
            let max = deltas[n - 1];
            let p50 = percentile(&deltas, 0.50);
            let p95 = percentile(&deltas, 0.95);
            let p99 = percentile(&deltas, 0.99);

            println!(
                "  {} -> {} ({n}/{a_total} matched, {match_pct:.0}%):  min={}  avg={}  p50={}  p95={}  p99={}  max={}",
                clients[i].name,
                clients[j].name,
                format_us(min),
                format_us(mean),
                format_us(p50),
                format_us(p95),
                format_us(p99),
                format_us(max),
            );
        }
    }
}

fn write_csv(clients: &[ClientData], path: &str) -> Result<()> {
    let mut f = std::fs::File::create(path)?;

    // Header
    write!(f, "content_hash")?;
    for c in clients {
        write!(
            f,
            ",{}_pts_ms,{}_arrival_us,{}_bytes",
            c.name, c.name, c.name
        )?;
    }
    writeln!(f)?;

    // Collect all unique hashes
    let mut all_hashes: Vec<u64> = clients
        .iter()
        .flat_map(|c| c.samples.keys().copied())
        .collect();
    all_hashes.sort();
    all_hashes.dedup();

    let base_instant = clients
        .iter()
        .flat_map(|c| c.arrivals.first().map(|&(inst, _)| inst))
        .min();
    let Some(base_instant) = base_instant else {
        return Ok(());
    };

    for hash in all_hashes {
        write!(f, "{hash:016x}")?;
        for c in clients {
            if let Some(&(pts, arrival, sz)) = c.samples.get(&hash) {
                let us = arrival.duration_since(base_instant).as_micros();
                write!(f, ",{pts},{us},{sz}")?;
            } else {
                write!(f, ",,,")?;
            }
        }
        writeln!(f)?;
    }

    eprintln!("CSV written to {path}");
    Ok(())
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    gst::init()?;

    let args = Args::parse();

    let n_clients =
        args.rtsp_urls.len() + args.udp_endpoints.len() + usize::from(args.webrtc_url.is_some());
    if n_clients < 2 {
        return Err(anyhow!(
            "At least two clients are required for latency comparison.\n\
             Use --rtsp, --webrtc, and/or --udp to add clients."
        ));
    }

    let mut client_data: Vec<ClientData> = Vec::new();
    let mut pipelines: Vec<gst::Pipeline> = Vec::new();
    let mut signaling_tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    // Create RTSP clients
    for (i, url) in args.rtsp_urls.iter().enumerate() {
        let name = format!("rtsp-{i}");
        let (tx, rx) = mpsc::unbounded_channel();
        let pipeline = rtsp_client::create_rtsp_client(&name, url, tx)?;
        eprintln!("[{name}] Created for {url}");
        client_data.push(ClientData {
            name,
            receiver: rx,
            samples: HashMap::new(),
            arrivals: Vec::new(),
        });
        pipelines.push(pipeline);
    }

    // Create UDP clients
    for (i, endpoint) in args.udp_endpoints.iter().enumerate() {
        let name = format!("udp-{i}");
        let (addr, port_str) = endpoint
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("Invalid UDP endpoint '{endpoint}', expected ADDR:PORT"))?;
        let port: i32 = port_str.parse()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let pipeline = udp_client::create_udp_client(&name, addr, port, args.codec, tx)?;
        eprintln!("[{name}] Created for {endpoint}");
        client_data.push(ClientData {
            name,
            receiver: rx,
            samples: HashMap::new(),
            arrivals: Vec::new(),
        });
        pipelines.push(pipeline);
    }

    // Create WebRTC client
    if let Some(ref ws_url) = args.webrtc_url {
        let name = "webrtc-0".to_string();
        let (tx, rx) = mpsc::unbounded_channel();
        let (pipeline, task) =
            webrtc_client::create_webrtc_client(&name, ws_url, args.producer_id, tx).await?;
        eprintln!("[{name}] Created for {ws_url}");
        client_data.push(ClientData {
            name,
            receiver: rx,
            samples: HashMap::new(),
            arrivals: Vec::new(),
        });
        pipelines.push(pipeline);
        signaling_tasks.push(task);
    }

    // Start all pipelines
    for pipeline in &pipelines {
        pipeline.set_state(gst::State::Playing)?;
    }
    eprintln!(
        "\nAll {} clients started. Measuring for {}s (warmup: {}s)...\n",
        n_clients, args.duration, args.warmup
    );

    let start = Instant::now();
    let warmup = Duration::from_secs(args.warmup);
    let duration = Duration::from_secs(args.duration);
    let report_interval = Duration::from_secs(args.report_interval);

    // Measurement loop
    let mut next_report = start + warmup + report_interval;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nInterrupted.");
                break;
            }
        }

        let elapsed = start.elapsed();
        if elapsed >= duration {
            break;
        }

        drain_samples(&mut client_data);

        // Discard warmup samples
        if elapsed < warmup {
            for c in &mut client_data {
                c.samples.clear();
                c.arrivals.clear();
            }
            continue;
        }

        // Periodic report
        if Instant::now() >= next_report {
            print_report(
                &client_data,
                &format!("Report @ {:.0}s", elapsed.as_secs_f64()),
            );
            next_report = Instant::now() + report_interval;
        }
    }

    // Final drain
    drain_samples(&mut client_data);

    // Final report
    print_report(&client_data, "Final Report");

    // CSV output
    if let Some(ref csv_path) = args.csv {
        write_csv(&client_data, csv_path)?;
    }

    // Shutdown
    eprintln!("\nShutting down...");
    for pipeline in &pipelines {
        pipeline.set_state(gst::State::Null).ok();
    }
    for task in signaling_tasks {
        task.abort();
    }

    Ok(())
}
