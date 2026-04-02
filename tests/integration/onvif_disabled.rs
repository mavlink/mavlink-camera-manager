use reqwest::StatusCode;

use crate::common::mcm::McmProcess;

#[tokio::test]
async fn onvif_endpoints_return_not_found_when_disabled() {
    let mcm = McmProcess::start().await.unwrap();
    let client = reqwest::Client::new();
    let base_url = mcm.rest_url();

    let devices = client
        .get(format!("{base_url}/onvif/devices"))
        .send()
        .await
        .unwrap();
    assert_eq!(devices.status(), StatusCode::NOT_FOUND);

    let authenticate = client
        .post(format!("{base_url}/onvif/authentication"))
        .query(&[
            ("device_uuid", uuid::Uuid::nil().to_string()),
            ("username", "user".to_string()),
            ("password", "password".to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(authenticate.status(), StatusCode::NOT_FOUND);

    let unauthenticate = client
        .delete(format!("{base_url}/onvif/authentication"))
        .query(&[("device_uuid", uuid::Uuid::nil().to_string())])
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticate.status(), StatusCode::NOT_FOUND);
}
