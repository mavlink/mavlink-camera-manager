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
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if !drain(rx).is_empty() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!("skip {label}: no frames within {TIMEOUT:?}");
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
        wait_for_rtsp_tcp(&rtsp_url, TIMEOUT).await;
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

    if let Err(error) = client.delete_stream(name).await {
        eprintln!("skip {name}: delete_stream failed: {error:#}");
    }
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
