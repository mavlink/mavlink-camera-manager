use super::*;

#[tokio::test]
async fn test_udp_redirect_thumbnail() {
    let (_mcm, client, mut sender) = setup_udp_redirect().await;

    let body = wait_for_thumbnail(&client, "Redirect", TIMEOUT).await;
    assert!(
        body.len() > 100,
        "thumbnail body too small ({} bytes), expected a JPEG image",
        body.len()
    );
    sender.kill().ok();
}

#[tokio::test]
async fn test_rtsp_redirect_thumbnail() {
    let (_mcm, client) = setup_fake_rtsp_and_redirect("test_redir_thumb").await;

    let body = wait_for_thumbnail(&client, "Redirect", TIMEOUT).await;
    assert!(
        body.len() > 100,
        "thumbnail body too small ({} bytes), expected a JPEG image",
        body.len()
    );
}

#[tokio::test]
async fn test_h265_udp_redirect_thumbnail() {
    let (_mcm, client, mut sender) = setup_h265_udp_redirect().await;

    let body = wait_for_thumbnail(&client, "Redirect", TIMEOUT).await;
    assert!(
        body.len() > 100,
        "thumbnail body too small ({} bytes), expected a JPEG image",
        body.len()
    );
    sender.kill().ok();
}

#[tokio::test]
async fn test_h265_rtsp_redirect_thumbnail() {
    let (_mcm, client) = setup_fake_h265_rtsp_and_redirect("test_h265_redir_thumb").await;

    let body = wait_for_thumbnail(&client, "Redirect", TIMEOUT).await;
    assert!(
        body.len() > 100,
        "thumbnail body too small ({} bytes), expected a JPEG image",
        body.len()
    );
}
