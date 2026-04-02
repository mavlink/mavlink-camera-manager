use std::time::Duration;

use anyhow::Result;

pub struct ThumbnailClient {
    client: reqwest::Client,
    base_url: String,
}

impl ThumbnailClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.to_string(),
        }
    }

    pub async fn thumbnail(&self, source: &str) -> Result<reqwest::Response> {
        Ok(self
            .client
            .get(format!("{}/thumbnail", self.base_url))
            .query(&[("source", source)])
            .send()
            .await?)
    }

    pub async fn thumbnail_with_retry(&self, source: &str, retries: u32) -> reqwest::Response {
        let mut last_err = None;
        for attempt in 0..retries {
            match self.thumbnail(source).await {
                Ok(resp) => return resp,
                Err(e) => {
                    eprintln!(
                        "thumbnail request attempt {}/{retries} failed: {e}",
                        attempt + 1
                    );
                    last_err = Some(e);
                    if attempt + 1 < retries {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
        panic!(
            "thumbnail request failed after {retries} attempts: {}",
            last_err.unwrap()
        );
    }

    pub async fn cold_thumbnail(&self, source: &str, timeout: Duration) -> Vec<u8> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let resp = match self.thumbnail(source).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("cold_thumbnail request failed (transient): {e}");
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "cold thumbnail request never succeeded: {e}"
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if resp.status() == 200 {
                return resp.bytes().await.unwrap().to_vec();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "cold thumbnail never returned 200 (got {})",
                resp.status()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn ensure_data_flowing(&self, source: &str, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let resp = match self.thumbnail(source).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("thumbnail request failed (transient): {e}");
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "thumbnail request never succeeded: {e}"
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if resp.status() == 200 {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "thumbnail never returned 200 (data not flowing, last status: {})",
                resp.status()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
