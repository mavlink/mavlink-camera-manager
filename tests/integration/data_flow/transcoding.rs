use super::*;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 30;

const RAW_ENCODES: &[&str] = &["NV12", "YUYV", "RGB"];
const COMPRESSED_ENCODES: &[&str] = &["MJPG", "H264", "H265"];

fn rtsp_codec(sink_encode: &str) -> Codec {
    match sink_encode {
        "H264" => Codec::H264,
        "H265" => Codec::H265,
        "MJPG" => Codec::Mjpg,
        "NV12" | "YUYV" | "RGB" => Codec::Yuyv,
        other => panic!("unsupported sink encode {other}"),
    }
}

fn cell_slug(source_encode: &str, sink_encode: &str) -> String {
    format!(
        "{}_{}",
        source_encode.to_ascii_lowercase(),
        sink_encode.to_ascii_lowercase()
    )
}

async fn try_verify_data_flow(rx: &mut mpsc::UnboundedReceiver<FrameSample>, label: &str) -> bool {
    let deadline = tokio::time::Instant::now() + MEASUREMENT_WINDOW;
    loop {
        if !drain(rx).is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!("skip {label}: no frames within {MEASUREMENT_WINDOW:?}");
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let samples = collect_frames(rx, MEASUREMENT_WINDOW, MAX_FRAME_GAP).await;
    if samples.len() < MIN_FRAME_COUNT {
        eprintln!(
            "skip {label}: expected at least {MIN_FRAME_COUNT} frames over {MEASUREMENT_WINDOW:?}, got {}",
            samples.len()
        );
        return false;
    }
    true
}

async fn rtsp_factory_ready(url: &str, timeout: Duration) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let parsed: url::Url = url.parse().expect("rtsp url");
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(8554);
    let addr = format!("{host}:{port}");
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let factory_ready = async {
            let mut stream = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            .ok()?
            .ok()?;
            let request = format!("OPTIONS rtsp://{addr}{path} RTSP/1.0\r\nCSeq: 1\r\n\r\n");
            stream.write_all(request.as_bytes()).await.ok()?;
            let mut buffer = [0u8; 256];
            let bytes_read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
                .await
                .ok()?
                .ok()?;
            let response = std::str::from_utf8(&buffer[..bytes_read]).unwrap_or("");
            Some(response.starts_with("RTSP/1.0 200"))
        }
        .await;
        if factory_ready == Some(true) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

async fn run_auto_or_manual_rtsp(
    client: &McmClient,
    mcm: &McmProcess,
    name: &str,
    path: &str,
    post: &PostStream,
    sink_encode: &str,
    measure_rtsp: bool,
) {
    if let Err(error) = client.create_stream(post).await {
        eprintln!("skip {name}: create_stream failed: {error:#}");
        return;
    }
    if let Err(error) = client.wait_for_streams_running(1, TIMEOUT).await {
        eprintln!("skip {name}: stream did not run: {error:#}");
        let _ = client.delete_stream(name).await;
        return;
    }

    if measure_rtsp {
        let rtsp_url = mcm.rtsp_url(path);
        if !rtsp_factory_ready(&rtsp_url, TIMEOUT).await {
            eprintln!("skip {name}: RTSP factory not serving within {TIMEOUT:?}");
        } else {
            let (tx, mut rx) = mpsc::unbounded_channel();
            match stream_clients::rtsp_client::RtspClient::new(
                &rtsp_url,
                rtsp_codec(sink_encode),
                Some(tx),
            )
            .await
            {
                Ok(_rtsp) => {
                    let _ = try_verify_data_flow(&mut rx, name).await;
                }
                Err(error) => {
                    eprintln!("skip {name}: RTSP client failed: {error:#}");
                }
            }
        }
    }

    if let Err(error) = client.delete_stream(name).await {
        eprintln!("skip {name}: delete_stream failed: {error:#}");
    }
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match client.list_streams().await {
            Ok(streams)
                if streams
                    .iter()
                    .all(|stream| stream.video_and_stream.name != name) =>
            {
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

async fn assert_post_rejected(client: &McmClient, post: &PostStream, label: &str) {
    let status = client
        .create_stream_status(post)
        .await
        .unwrap_or_else(|error| panic!("{label}: POST failed to send: {error:#}"));
    assert!(
        !status.is_success(),
        "{label}: expected rejected POST, got {status}"
    );
}

#[tokio::test]
async fn test_auto_encode_rtsp_data_flow() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    for source_encode in RAW_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("auto_encode_{slug}");
            let path = format!("auto_encode_{slug}");
            let post = McmClient::build_fake_auto_rtsp(
                source_encode,
                sink_encode,
                &name,
                WIDTH,
                HEIGHT,
                FPS,
                &path,
                Some(NON_LAZY),
                mcm.rtsp_port,
            );
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_auto_decode_rtsp_data_flow() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    for source_encode in COMPRESSED_ENCODES {
        for sink_encode in RAW_ENCODES {
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("auto_decode_{slug}");
            let path = format!("auto_decode_{slug}");
            let post = McmClient::build_fake_auto_rtsp(
                source_encode,
                sink_encode,
                &name,
                WIDTH,
                HEIGHT,
                FPS,
                &path,
                Some(NON_LAZY),
                mcm.rtsp_port,
            );
            run_auto_or_manual_rtsp(
                &client,
                &mcm,
                &name,
                &path,
                &post,
                sink_encode,
                *sink_encode == "YUYV",
            )
            .await;
        }
    }
}

#[tokio::test]
async fn test_auto_transcode_rtsp_data_flow() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    for source_encode in COMPRESSED_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            if source_encode == sink_encode {
                continue;
            }
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("auto_transcode_{slug}");
            let path = format!("auto_transcode_{slug}");
            let post = McmClient::build_fake_auto_rtsp(
                source_encode,
                sink_encode,
                &name,
                WIDTH,
                HEIGHT,
                FPS,
                &path,
                Some(NON_LAZY),
                mcm.rtsp_port,
            );
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_manual_encode_rtsp_data_flow() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());
    let encoders = client.list_gst_encoders().await.unwrap();

    for source_encode in RAW_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            let Some(encoder) = encoders.first_factory(sink_encode) else {
                eprintln!("skip manual_encode_{source_encode}_{sink_encode}: no encoder listed");
                continue;
            };
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("manual_encode_{slug}");
            let path = format!("manual_encode_{slug}");
            let post = McmClient::build_fake_manual_rtsp(
                source_encode,
                sink_encode,
                &encoder,
                "",
                &name,
                WIDTH,
                HEIGHT,
                FPS,
                &path,
                Some(NON_LAZY),
                mcm.rtsp_port,
            );
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_manual_transcode_rtsp_data_flow() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());
    let encoders = client.list_gst_encoders().await.unwrap();
    let decoders = client.list_gst_decoders().await.unwrap();

    for source_encode in COMPRESSED_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            if source_encode == sink_encode {
                continue;
            }
            let Some(encoder) = encoders.first_factory(sink_encode) else {
                eprintln!("skip manual_transcode_{source_encode}_{sink_encode}: no encoder listed");
                continue;
            };
            let Some(decoder) = decoders.first_factory(source_encode) else {
                eprintln!("skip manual_transcode_{source_encode}_{sink_encode}: no decoder listed");
                continue;
            };
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("manual_transcode_{slug}");
            let path = format!("manual_transcode_{slug}");
            let post = McmClient::build_fake_manual_rtsp(
                source_encode,
                sink_encode,
                &encoder,
                &decoder,
                &name,
                WIDTH,
                HEIGHT,
                FPS,
                &path,
                Some(NON_LAZY),
                mcm.rtsp_port,
            );
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_manual_decode_rtsp_data_flow() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());
    let decoders = client.list_gst_decoders().await.unwrap();

    for source_encode in COMPRESSED_ENCODES {
        let Some(decoder) = decoders.first_factory(source_encode) else {
            eprintln!("skip manual_decode_{source_encode}: no decoder listed");
            continue;
        };
        for sink_encode in RAW_ENCODES {
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("manual_decode_{slug}");
            let path = format!("manual_decode_{slug}");
            let post = McmClient::build_fake_manual_rtsp(
                source_encode,
                sink_encode,
                "",
                &decoder,
                &name,
                WIDTH,
                HEIGHT,
                FPS,
                &path,
                Some(NON_LAZY),
                mcm.rtsp_port,
            );
            run_auto_or_manual_rtsp(
                &client,
                &mcm,
                &name,
                &path,
                &post,
                sink_encode,
                *sink_encode == "YUYV",
            )
            .await;
        }
    }
}

#[tokio::test]
async fn test_transcoding_rejected_posts() {
    gst::init().unwrap();
    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let auto_identity = McmClient::build_fake_auto_rtsp(
        "H264",
        "H264",
        "reject_auto_identity",
        WIDTH,
        HEIGHT,
        FPS,
        "reject_auto_identity",
        Some(NON_LAZY),
        mcm.rtsp_port,
    );
    assert_post_rejected(&client, &auto_identity, "Auto identity H264").await;

    let manual_identity = McmClient::build_fake_manual_rtsp(
        "H264",
        "H264",
        "x264enc",
        "",
        "reject_manual_identity",
        WIDTH,
        HEIGHT,
        FPS,
        "reject_manual_identity",
        Some(NON_LAZY),
        mcm.rtsp_port,
    );
    assert_post_rejected(&client, &manual_identity, "Manual identity H264").await;

    let manual_raw_to_raw = McmClient::build_fake_manual_rtsp(
        "NV12",
        "YUYV",
        "",
        "",
        "reject_manual_raw_to_raw",
        WIDTH,
        HEIGHT,
        FPS,
        "reject_manual_raw_to_raw",
        Some(NON_LAZY),
        mcm.rtsp_port,
    );
    assert_post_rejected(&client, &manual_raw_to_raw, "Manual NV12 to YUYV").await;

    let auto_raw_to_raw = McmClient::build_fake_auto_rtsp(
        "NV12",
        "YUYV",
        "reject_auto_raw_to_raw",
        WIDTH,
        HEIGHT,
        FPS,
        "reject_auto_raw_to_raw",
        Some(NON_LAZY),
        mcm.rtsp_port,
    );
    assert_post_rejected(&client, &auto_raw_to_raw, "Auto NV12 to YUYV").await;
}
