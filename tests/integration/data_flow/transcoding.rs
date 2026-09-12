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

fn gst_factory_missing(factory_name: &str) -> bool {
    gst::ElementFactory::find(factory_name).is_none()
}

fn fake_compressed_source_encoder(source_encode: &str) -> Option<&'static str> {
    match source_encode {
        "H264" => Some("x264enc"),
        "H265" => Some("x265enc"),
        "MJPG" => Some("jpegenc"),
        _ => None,
    }
}

fn skip_missing_factories(name: &str, factory_names: &[&str]) -> bool {
    for factory_name in factory_names {
        if gst_factory_missing(factory_name) {
            eprintln!("skip {name}: missing GStreamer factory {factory_name}");
            return true;
        }
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
    client
        .create_stream(post)
        .await
        .unwrap_or_else(|error| panic!("{name}: create_stream failed: {error:#}"));
    client
        .wait_for_streams_running(1, TIMEOUT)
        .await
        .unwrap_or_else(|error| panic!("{name}: stream did not run: {error:#}"));

    if measure_rtsp {
        let rtsp_url = mcm.rtsp_url(path);
        wait_for_rtsp_tcp(&rtsp_url, TIMEOUT)
            .await
            .unwrap_or_else(|error| panic!("{name}: RTSP factory not ready: {error:#}"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _rtsp = stream_clients::rtsp_client::RtspClient::new(
            &rtsp_url,
            rtsp_codec(sink_encode),
            Some(tx),
            TCP_CONNECT,
        )
        .await
        .unwrap_or_else(|error| panic!("{name}: RTSP client failed: {error:#}"));
        verify_data_flow(&mut rx, name).await;
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

    for source_encode in RAW_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("auto_encode_{slug}");
            if skip_missing_factories(&name, &["videotestsrc", "videoconvert", "encodebin"]) {
                continue;
            }
            let mcm = McmProcess::start().await.unwrap();
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
            let client = McmClient::new(&mcm.rest_url());
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_auto_decode_rtsp_data_flow() {
    gst::init().unwrap();

    for source_encode in COMPRESSED_ENCODES {
        for sink_encode in RAW_ENCODES {
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("auto_decode_{slug}");
            let mut required = vec!["videotestsrc", "videoconvert", "decodebin"];
            if let Some(encoder) = fake_compressed_source_encoder(source_encode) {
                required.push(encoder);
            }
            if skip_missing_factories(&name, &required) {
                continue;
            }
            let mcm = McmProcess::start().await.unwrap();
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
            let client = McmClient::new(&mcm.rest_url());
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

    for source_encode in COMPRESSED_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            if source_encode == sink_encode {
                continue;
            }
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("auto_transcode_{slug}");
            let mut required = vec!["videotestsrc", "videoconvert", "encodebin", "decodebin"];
            if let Some(encoder) = fake_compressed_source_encoder(source_encode) {
                required.push(encoder);
            }
            if skip_missing_factories(&name, &required) {
                continue;
            }
            let mcm = McmProcess::start().await.unwrap();
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
            let client = McmClient::new(&mcm.rest_url());
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_manual_encode_rtsp_data_flow() {
    gst::init().unwrap();
    let encoder_probe = McmProcess::start().await.unwrap();
    let encoder_client = McmClient::new(&encoder_probe.rest_url());
    let encoders = encoder_client.list_gst_encoders().await.unwrap();
    drop(encoder_probe);

    for source_encode in RAW_ENCODES {
        for sink_encode in COMPRESSED_ENCODES {
            let Some(encoder) = encoders.first_factory(sink_encode) else {
                eprintln!("skip manual_encode_{source_encode}_{sink_encode}: no encoder listed");
                continue;
            };
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("manual_encode_{slug}");
            if skip_missing_factories(&name, &["videotestsrc", "videoconvert", &encoder]) {
                continue;
            }
            let mcm = McmProcess::start().await.unwrap();
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
            let client = McmClient::new(&mcm.rest_url());
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_manual_transcode_rtsp_data_flow() {
    gst::init().unwrap();
    let probe = McmProcess::start().await.unwrap();
    let probe_client = McmClient::new(&probe.rest_url());
    let encoders = probe_client.list_gst_encoders().await.unwrap();
    let decoders = probe_client.list_gst_decoders().await.unwrap();
    drop(probe);

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
            let mut required = vec![
                "videotestsrc",
                "videoconvert",
                encoder.as_str(),
                decoder.as_str(),
            ];
            if let Some(source_encoder) = fake_compressed_source_encoder(source_encode) {
                required.push(source_encoder);
            }
            if skip_missing_factories(&name, &required) {
                continue;
            }
            let mcm = McmProcess::start().await.unwrap();
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
            let client = McmClient::new(&mcm.rest_url());
            run_auto_or_manual_rtsp(&client, &mcm, &name, &path, &post, sink_encode, true).await;
        }
    }
}

#[tokio::test]
async fn test_manual_decode_rtsp_data_flow() {
    gst::init().unwrap();
    let probe = McmProcess::start().await.unwrap();
    let probe_client = McmClient::new(&probe.rest_url());
    let decoders = probe_client.list_gst_decoders().await.unwrap();
    drop(probe);

    for source_encode in COMPRESSED_ENCODES {
        let Some(decoder) = decoders.first_factory(source_encode) else {
            eprintln!("skip manual_decode_{source_encode}: no decoder listed");
            continue;
        };
        for sink_encode in RAW_ENCODES {
            let slug = cell_slug(source_encode, sink_encode);
            let name = format!("manual_decode_{slug}");
            let mut required = vec!["videotestsrc", "videoconvert", decoder.as_str()];
            if let Some(source_encoder) = fake_compressed_source_encoder(source_encode) {
                required.push(source_encoder);
            }
            if skip_missing_factories(&name, &required) {
                continue;
            }
            let mcm = McmProcess::start().await.unwrap();
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
            let client = McmClient::new(&mcm.rest_url());
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
