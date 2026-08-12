use super::*;

/// External gst-launch UDP sender -> redirect stream -> WebRTC client
/// receives actual decoded frames.
async fn run_redirect_webrtc_data_flow(codec: Codec) {
    gst::init().unwrap();

    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_udp_sender(codec, None, "127.0.0.1", udp_port);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let stream_name = match codec {
        Codec::H264 => "redirect_webrtc",
        Codec::H265 => "redirect_h265_webrtc",
        other => unreachable!("Redirect pipeline does not support {other:?}"),
    };
    let redirect = PostStream {
        name: stream_name.to_string(),
        source: "Redirect".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![Url::parse(&format!("udp://127.0.0.1:{udp_port}")).unwrap()],
            configuration: CaptureConfiguration::Redirect {},
            extended_configuration: Some(ExtendedConfiguration {
                disable_lazy: true,
                disable_mavlink: true,
                disable_zenoh: true,
                ..Default::default()
            }),
        },
    };
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create redirect stream: {e}");
    });

    client
        .wait_for_streams_running(1, TIMEOUT)
        .await
        .unwrap_or_else(|e| {
            sender.kill().ok();
            panic!("redirect stream not running: {e}");
        });

    // Allow the redirect pipeline to fully resolve codec and settle
    tokio::time::sleep(Duration::from_secs(5)).await;

    let signalling_url = mcm.signalling_url();
    let overall_deadline = tokio::time::Instant::now() + Duration::from_secs(60);

    // Retry loop: the redirect's DTLS may close on the first attempt if the
    // media pipeline isn't ready yet. Recreate everything on each retry.
    let (mut rx, _webrtc) = loop {
        if tokio::time::Instant::now() >= overall_deadline {
            sender.kill().ok();
            panic!("Redirect WebRTC: no frames received within 60s (all attempts failed)");
        }

        let (tx, mut attempt_rx) = mpsc::unbounded_channel();
        let client = match stream_clients::webrtc_client::WebrtcClient::connect(
            &signalling_url,
            None,
            Some(tx),
            None,
        )
        .await
        {
            Ok(c) => c,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        // Give ample time: DTLS + ICE negotiation can take ~10s on redirect
        // streams, and media needs a few more seconds to start flowing.
        let attempt_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let got_frame = loop {
            if !drain(&mut attempt_rx).is_empty() {
                break true;
            }
            if tokio::time::Instant::now() >= attempt_deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };

        if got_frame {
            break (attempt_rx, client);
        }

        drop(client);
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    verify_data_flow(&mut rx, "redirect WebRTC").await;
    sender.kill().ok();
}

#[tokio::test]
async fn test_redirect_h264_webrtc_data_flow() {
    run_redirect_webrtc_data_flow(Codec::H264).await;
}

#[tokio::test]
async fn test_redirect_h265_webrtc_data_flow() {
    run_redirect_webrtc_data_flow(Codec::H265).await;
}

/// Fake RTSP source -> redirect RTSP stream -> RTSP client receives
/// actual decoded frames via the shared RTSP endpoint.
/// When `profile` is `Some`, an external `TestRtspServer` is used instead
/// of the MCM built-in fake RTSP source.
async fn run_redirect_rtsp_data_flow(codec: Codec, profile: Option<&str>) {
    gst::init().unwrap();

    if let Some(profile) = profile {
        let test_port = allocate_ports(1).unwrap()[0];
        let path = format!("redir_{:?}_{}_rtsp", codec, profile.replace('-', "_"));

        let _rtsp_server = TestRtspServer::start(codec, Some(profile), test_port, &path);

        tokio::time::sleep(Duration::from_secs(2)).await;

        let mcm = McmProcess::start().await.unwrap();
        let client = McmClient::new(&mcm.rest_url());

        let redirect_name = format!("redirect_{:?}_{}_rtsp", codec, profile.replace('-', "_"));
        let redirect =
            McmClient::build_redirect_rtsp(&redirect_name, "127.0.0.1", test_port, &path);
        client.create_stream(&redirect).await.unwrap();

        client
            .wait_for_streams_running(1, Duration::from_secs(60))
            .await
            .expect("redirect stream should be running");

        let rtsp_url = _rtsp_server.rtsp_url(&path);
        wait_for_rtsp_tcp(&rtsp_url, TIMEOUT).await;

        let (rtsp_tx, mut rtsp_rx) = mpsc::unbounded_channel();
        let _rtsp = stream_clients::rtsp_client::RtspClient::new(&rtsp_url, codec, Some(rtsp_tx))
            .await
            .unwrap();

        verify_data_flow(&mut rtsp_rx, &format!("RTSP redirect {codec:?} {profile}")).await;
    } else {
        let mcm = McmProcess::start().await.unwrap();
        let client = McmClient::new(&mcm.rest_url());

        let (sender_name, path, redirect_name) = match codec {
            Codec::H264 => ("fake_rtsp_sender", "redirect_src", "redirect_rtsp"),
            Codec::H265 => (
                "fake_h265_rtsp_sender",
                "redirect_h265_src",
                "redirect_h265_rtsp",
            ),
            other => unreachable!("Redirect pipeline does not support {other:?}"),
        };
        let fake = build_fake_rtsp(codec, sender_name, 160, 120, 30, path, None, mcm.rtsp_port);
        client.create_stream(&fake).await.unwrap();
        client
            .wait_for_stream_idle(sender_name, TIMEOUT)
            .await
            .expect("fake RTSP sender should complete initial lifecycle");
        mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

        let redirect =
            McmClient::build_redirect_rtsp(redirect_name, "127.0.0.1", mcm.rtsp_port, path);
        client.create_stream(&redirect).await.unwrap();
        client
            .wait_for_stream_idle(redirect_name, TIMEOUT)
            .await
            .expect("redirect RTSP should complete initial lifecycle");

        let rtsp_url = mcm.rtsp_url(path);
        wait_for_rtsp_tcp(&rtsp_url, TIMEOUT).await;

        let (rtsp_tx, mut rtsp_rx) = mpsc::unbounded_channel();
        let _rtsp = stream_clients::rtsp_client::RtspClient::new(&rtsp_url, codec, Some(rtsp_tx))
            .await
            .unwrap();

        verify_data_flow(&mut rtsp_rx, "RTSP (redirect)").await;
    }
}

#[tokio::test]
async fn test_redirect_h264_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H264, None).await;
}

#[tokio::test]
async fn test_redirect_h265_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H265, None).await;
}

/// External UDP sender -> redirect stream -> thumbnail returns a JPEG image.
/// When `profile` is `Some`, the sender forces that codec profile.
async fn run_redirect_thumbnail_data_flow(codec: Codec, profile: Option<&str>) {
    let udp_port = allocate_udp_ports(1).unwrap()[0];
    let mut sender = spawn_udp_sender(codec, profile, "127.0.0.1", udp_port);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let stream_name = match profile {
        Some(p) => format!("redir_{:?}_{}_thumb", codec, p.replace('-', "_")),
        None => match codec {
            Codec::H264 => "redirect_thumb".to_string(),
            Codec::H265 => "redirect_h265_thumb".to_string(),
            other => unreachable!("Redirect pipeline does not support {other:?}"),
        },
    };
    let redirect = McmClient::build_redirect_udp(&stream_name, "127.0.0.1", udp_port);
    client.create_stream(&redirect).await.unwrap_or_else(|e| {
        sender.kill().ok();
        panic!("failed to create redirect stream: {e}");
    });

    client
        .wait_for_streams_running(1, Duration::from_secs(60))
        .await
        .unwrap_or_else(|e| {
            sender.kill().ok();
            panic!("redirect stream not running: {e}");
        });

    let body = wait_for_thumbnail(&client, "Redirect", Duration::from_secs(60)).await;
    assert!(
        body.len() > 100,
        "Thumbnail too small ({} bytes), expected a JPEG image",
        body.len()
    );
    assert_eq!(&body[..2], &[0xFF, 0xD8], "Thumbnail is not a JPEG image");

    sender.kill().ok();
}

#[tokio::test]
async fn test_redirect_h264_thumbnail_data_flow() {
    run_redirect_thumbnail_data_flow(Codec::H264, None).await;
}

#[tokio::test]
async fn test_redirect_h265_thumbnail_data_flow() {
    run_redirect_thumbnail_data_flow(Codec::H265, None).await;
}

/// Fake RTSP source -> redirect RTSP stream -> thumbnail returns a JPEG image.
/// When `profile` is `Some`, an external `TestRtspServer` is used instead
/// of the MCM built-in fake RTSP source.
async fn run_redirect_rtsp_thumbnail_data_flow(codec: Codec, profile: Option<&str>) {
    if let Some(profile) = profile {
        gst::init().unwrap();

        let test_port = allocate_ports(1).unwrap()[0];
        let path = format!("redir_{:?}_{}_thumb", codec, profile.replace('-', "_"));

        let _rtsp_server = TestRtspServer::start(codec, Some(profile), test_port, &path);

        tokio::time::sleep(Duration::from_secs(2)).await;

        let mcm = McmProcess::start().await.unwrap();
        let client = McmClient::new(&mcm.rest_url());

        let redirect_name = format!("redirect_{:?}_{}_thumb", codec, profile.replace('-', "_"));
        let redirect =
            McmClient::build_redirect_rtsp(&redirect_name, "127.0.0.1", test_port, &path);
        client.create_stream(&redirect).await.unwrap();

        client
            .wait_for_streams_running(1, Duration::from_secs(60))
            .await
            .expect("redirect stream should be running");

        let body = wait_for_thumbnail(&client, "Redirect", Duration::from_secs(60)).await;
        assert!(
            body.len() > 100,
            "Thumbnail too small ({} bytes), expected a JPEG image",
            body.len()
        );
        assert_eq!(&body[..2], &[0xFF, 0xD8], "Thumbnail is not a JPEG image");
    } else {
        let mcm = McmProcess::start().await.unwrap();
        let client = McmClient::new(&mcm.rest_url());

        let (sender_name, path, redirect_name) = match codec {
            Codec::H264 => (
                "fake_rtsp_thumb_sender",
                "redir_thumb_src",
                "redirect_rtsp_thumb",
            ),
            Codec::H265 => (
                "fake_h265_rtsp_thumb_sender",
                "redir_h265_thumb_src",
                "redirect_h265_rtsp_thumb",
            ),
            other => unreachable!("Redirect pipeline does not support {other:?}"),
        };
        let fake = build_fake_rtsp(codec, sender_name, 160, 120, 30, path, None, mcm.rtsp_port);
        client.create_stream(&fake).await.unwrap();
        client
            .wait_for_stream_idle(sender_name, TIMEOUT)
            .await
            .expect("fake RTSP sender should complete initial lifecycle");
        mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

        let redirect =
            McmClient::build_redirect_rtsp(redirect_name, "127.0.0.1", mcm.rtsp_port, path);
        client.create_stream(&redirect).await.unwrap();
        client
            .wait_for_stream_idle(redirect_name, TIMEOUT)
            .await
            .expect("redirect should complete initial lifecycle");

        let body = wait_for_thumbnail(&client, "Redirect", Duration::from_secs(60)).await;
        assert!(
            body.len() > 100,
            "Thumbnail too small ({} bytes), expected a JPEG image",
            body.len()
        );
        assert_eq!(&body[..2], &[0xFF, 0xD8], "Thumbnail is not a JPEG image");
    }
}

#[tokio::test]
async fn test_redirect_h264_rtsp_thumbnail_data_flow() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H264, None).await;
}

#[tokio::test]
async fn test_redirect_h265_rtsp_thumbnail_data_flow() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H265, None).await;
}

/// Fake RTSP source -> redirect RTSP stream -> WebRTC client receives
/// actual decoded frames.
async fn run_redirect_rtsp_webrtc_data_flow(codec: Codec) {
    gst::init().unwrap();

    let mcm = McmProcess::start().await.unwrap();
    let client = McmClient::new(&mcm.rest_url());

    let (sender_name, path, redirect_name) = match codec {
        Codec::H264 => (
            "fake_rtsp_webrtc_sender",
            "redir_webrtc_src",
            "redirect_rtsp_webrtc",
        ),
        Codec::H265 => (
            "fake_h265_rtsp_webrtc_sender",
            "redir_h265_webrtc_src",
            "redirect_h265_rtsp_webrtc",
        ),
        other => unreachable!("Redirect pipeline does not support {other:?}"),
    };
    let fake = build_fake_rtsp(codec, sender_name, 160, 120, 30, path, None, mcm.rtsp_port);
    client.create_stream(&fake).await.unwrap();
    client
        .wait_for_stream_idle(sender_name, TIMEOUT)
        .await
        .expect("fake RTSP sender should complete initial lifecycle");
    mcm.wait_for_rtsp_ready(path, TIMEOUT).await;

    let redirect = PostStream {
        name: redirect_name.to_string(),
        source: "Redirect".to_string(),
        stream_information: StreamInformation {
            endpoints: vec![
                Url::parse(&format!("rtsp://127.0.0.1:{}/{}", mcm.rtsp_port, path)).unwrap(),
            ],
            configuration: CaptureConfiguration::Redirect {},
            extended_configuration: Some(ExtendedConfiguration {
                disable_lazy: true,
                disable_mavlink: true,
                disable_zenoh: true,
                ..Default::default()
            }),
        },
    };
    client.create_stream(&redirect).await.unwrap();
    client
        .wait_for_streams_running(2, TIMEOUT)
        .await
        .expect("both streams should be running");

    tokio::time::sleep(Duration::from_secs(5)).await;

    let redirect_id = client
        .list_streams()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.video_and_stream.name == redirect_name)
        .expect("redirect stream should exist")
        .id;

    let signalling_url = mcm.signalling_url();
    let overall_deadline = tokio::time::Instant::now() + Duration::from_secs(60);

    let (mut rx, _webrtc) = loop {
        if tokio::time::Instant::now() >= overall_deadline {
            panic!("Redirect RTSP WebRTC: no frames received within 60s (all attempts failed)");
        }

        let (tx, mut attempt_rx) = mpsc::unbounded_channel();
        let client = match stream_clients::webrtc_client::WebrtcClient::connect(
            &signalling_url,
            Some(redirect_id),
            Some(tx),
            None,
        )
        .await
        {
            Ok(c) => c,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let attempt_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let got_frame = loop {
            if !drain(&mut attempt_rx).is_empty() {
                break true;
            }
            if tokio::time::Instant::now() >= attempt_deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };

        if got_frame {
            break (attempt_rx, client);
        }

        drop(client);
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    verify_data_flow(&mut rx, "redirect RTSP WebRTC").await;
}

#[tokio::test]
async fn test_redirect_h264_rtsp_webrtc_data_flow() {
    run_redirect_rtsp_webrtc_data_flow(Codec::H264).await;
}

#[tokio::test]
async fn test_redirect_h265_rtsp_webrtc_data_flow() {
    run_redirect_rtsp_webrtc_data_flow(Codec::H265).await;
}

#[tokio::test]
async fn test_redirect_h264_constrained_baseline_udp_thumbnail() {
    run_redirect_thumbnail_data_flow(Codec::H264, Some("constrained-baseline")).await;
}

#[tokio::test]
async fn test_redirect_h264_baseline_udp_thumbnail() {
    run_redirect_thumbnail_data_flow(Codec::H264, Some("baseline")).await;
}

#[tokio::test]
async fn test_redirect_h264_main_udp_thumbnail() {
    run_redirect_thumbnail_data_flow(Codec::H264, Some("main")).await;
}

#[tokio::test]
async fn test_redirect_h264_high_udp_thumbnail() {
    run_redirect_thumbnail_data_flow(Codec::H264, Some("high")).await;
}

#[tokio::test]
async fn test_redirect_h265_main10_udp_thumbnail() {
    run_redirect_thumbnail_data_flow(Codec::H265, Some("main-10")).await;
}

#[tokio::test]
async fn test_redirect_h264_constrained_baseline_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H264, Some("constrained-baseline")).await;
}

#[tokio::test]
async fn test_redirect_h264_baseline_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H264, Some("baseline")).await;
}

#[tokio::test]
async fn test_redirect_h264_main_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H264, Some("main")).await;
}

#[tokio::test]
async fn test_redirect_h264_high_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H264, Some("high")).await;
}

#[tokio::test]
async fn test_redirect_h265_main10_rtsp_data_flow() {
    run_redirect_rtsp_data_flow(Codec::H265, Some("main-10")).await;
}

#[tokio::test]
async fn test_redirect_h264_constrained_baseline_rtsp_thumbnail() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H264, Some("constrained-baseline")).await;
}

#[tokio::test]
async fn test_redirect_h264_baseline_rtsp_thumbnail() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H264, Some("baseline")).await;
}

#[tokio::test]
async fn test_redirect_h264_main_rtsp_thumbnail() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H264, Some("main")).await;
}

#[tokio::test]
async fn test_redirect_h264_high_rtsp_thumbnail() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H264, Some("high")).await;
}

#[tokio::test]
async fn test_redirect_h265_main10_rtsp_thumbnail() {
    run_redirect_rtsp_thumbnail_data_flow(Codec::H265, Some("main-10")).await;
}
