use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

pub struct ThumbnailClient {
    client: reqwest::Client,
    base_url: String,
}

impl ThumbnailClient {
    pub fn new(base_url: &str) -> Self {
        assert!(!base_url.is_empty(), "base_url must be non-empty");
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("building reqwest client"),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get(&self, source: &str) -> Result<reqwest::Response> {
        anyhow::ensure!(!source.is_empty(), "source must be non-empty");
        Ok(self
            .client
            .get(format!("{}/thumbnail", self.base_url))
            .query(&[("source", source)])
            .send()
            .await?)
    }

    pub async fn get_with_retry(&self, source: &str, retries: u32) -> Result<reqwest::Response> {
        anyhow::ensure!(retries > 0, "retries must be positive");
        let mut last_error = None;
        for attempt in 0..retries {
            match self.get(source).await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    warn!(
                        attempt = attempt + 1,
                        retries, %error, "thumbnail request failed"
                    );
                    last_error = Some(error);
                    if attempt + 1 < retries {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
        Err(last_error.unwrap())
            .context(format!("thumbnail request failed after {retries} attempts"))
    }

    pub async fn wait(&self, source: &str, timeout: Duration) -> Result<Vec<u8>> {
        anyhow::ensure!(!source.is_empty(), "source must be non-empty");
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last_status: Option<String> = None;
        loop {
            match self.get(source).await {
                Ok(response) => {
                    let status = response.status();
                    if status == 200 {
                        return Ok(response.bytes().await?.to_vec());
                    }
                    last_status = Some(format!("HTTP {status}"));
                    warn!(%status, "thumbnail not ready");
                }
                Err(error) => {
                    last_status = Some(error.to_string());
                    warn!(%error, "thumbnail request failed (transient)");
                }
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "thumbnail never returned 200 within {timeout:?} (last: {})",
                    last_status.as_deref().unwrap_or("no response")
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
