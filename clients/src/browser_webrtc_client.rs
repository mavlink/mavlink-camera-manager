use std::time::Duration;

use anyhow::{Context, Result};
use thirtyfour::{prelude::ElementQueryable, ChromiumLikeCapabilities};

pub struct BrowserWebrtcClient {
    driver: thirtyfour::WebDriver,
    _chromedriver: tokio::process::Child,
}

#[derive(Debug)]
pub struct BrowserWebrtcStats {
    pub frames_decoded: u64,
    pub frames_received: u64,
    pub frames_dropped: u64,
    pub key_frames_decoded: u64,
}

impl BrowserWebrtcClient {
    pub async fn new() -> Result<Self> {
        let port: u16 = std::env::var("CHROMEDRIVER_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(9515);

        let chromedriver = tokio::process::Command::new("chromedriver")
            .arg(format!("--port={port}"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn chromedriver -- is it installed?")?;

        tokio::time::sleep(Duration::from_secs(2)).await;

        let mut caps = thirtyfour::DesiredCapabilities::chrome();
        caps.set_headless().map_err(|e| anyhow::anyhow!("{e}"))?;
        caps.set_no_sandbox().map_err(|e| anyhow::anyhow!("{e}"))?;
        caps.set_disable_dev_shm_usage()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        caps.set_disable_web_security()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        caps.set_ignore_certificate_errors()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        caps.add_arg("--autoplay-policy=no-user-gesture-required")
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        caps.add_arg("--disable-gpu")
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let driver = thirtyfour::WebDriver::new(&format!("http://127.0.0.1:{port}"), caps)
            .await
            .map_err(|e| anyhow::anyhow!("WebDriver session failed: {e}"))?;

        Ok(Self {
            driver,
            _chromedriver: chromedriver,
        })
    }

    pub async fn connect(&self, url: &str, signalling_port: u16) -> Result<()> {
        self.driver
            .goto(url)
            .await
            .map_err(|e| anyhow::anyhow!("goto failed: {e}"))?;

        tokio::time::sleep(Duration::from_secs(2)).await;

        let port_input: thirtyfour::WebElement = self
            .driver
            .query(thirtyfour::By::Id("signallerPort"))
            .first()
            .await
            .map_err(|e| anyhow::anyhow!("signallerPort input not found: {e}"))?;
        port_input
            .clear()
            .await
            .map_err(|e| anyhow::anyhow!("clear signallerPort: {e}"))?;
        port_input
            .send_keys(&signalling_port.to_string())
            .await
            .map_err(|e| anyhow::anyhow!("set signallerPort: {e}"))?;

        let add_consumer: thirtyfour::WebElement = self
            .driver
            .query(thirtyfour::By::Id("add-consumer"))
            .first()
            .await
            .map_err(|e| anyhow::anyhow!("add-consumer not found: {e}"))?;
        add_consumer
            .click()
            .await
            .map_err(|e| anyhow::anyhow!("click add-consumer: {e}"))?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        let add_session: thirtyfour::WebElement = self
            .driver
            .query(thirtyfour::By::Id("add-session"))
            .first()
            .await
            .map_err(|e| anyhow::anyhow!("add-session not found: {e}"))?;
        add_session
            .click()
            .await
            .map_err(|e| anyhow::anyhow!("click add-session: {e}"))?;

        Ok(())
    }

    pub async fn wait_for_playing(&self, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let elements: std::result::Result<Vec<_>, _> = self
                .driver
                .query(thirtyfour::By::Id("session-status"))
                .with_text("Status: Playing")
                .all_from_selector()
                .await;

            if let Ok(elems) = elements {
                if !elems.is_empty() {
                    return Ok(());
                }
            }

            if tokio::time::Instant::now() > deadline {
                anyhow::bail!("Timed out waiting for 'Playing' status");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn get_stats(&self) -> Result<BrowserWebrtcStats> {
        let script = r#"
            const done = arguments[0];
            const pcs = window.__mcm_peer_connections || [];
            if (pcs.length === 0) {
                done({error: "no peer connections"});
                return;
            }
            const pc = pcs[pcs.length - 1];
            pc.getStats().then(stats => {
                let result = {
                    framesDecoded: 0,
                    framesReceived: 0,
                    framesDropped: 0,
                    keyFramesDecoded: 0
                };
                stats.forEach(report => {
                    if (report.type === 'inbound-rtp' && report.kind === 'video') {
                        result.framesDecoded = report.framesDecoded || 0;
                        result.framesReceived = report.framesReceived || 0;
                        result.framesDropped = report.framesDropped || 0;
                        result.keyFramesDecoded = report.keyFramesDecoded || 0;
                    }
                });
                done(result);
            }).catch(e => done({error: e.message}));
        "#;

        let ret = self
            .driver
            .execute_async(script, vec![])
            .await
            .map_err(|e| anyhow::anyhow!("execute getStats: {e}"))?;

        let val = ret.json();
        if let Some(err) = val.get("error") {
            anyhow::bail!("getStats JS error: {err}");
        }

        Ok(BrowserWebrtcStats {
            frames_decoded: val["framesDecoded"].as_u64().unwrap_or(0),
            frames_received: val["framesReceived"].as_u64().unwrap_or(0),
            frames_dropped: val["framesDropped"].as_u64().unwrap_or(0),
            key_frames_decoded: val["keyFramesDecoded"].as_u64().unwrap_or(0),
        })
    }

    pub async fn wait_for_decoded_frames(
        &self,
        min: u64,
        timeout: Duration,
    ) -> Result<BrowserWebrtcStats> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self.get_stats().await {
                Ok(stats) if stats.frames_decoded >= min => return Ok(stats),
                Ok(stats) => {
                    if tokio::time::Instant::now() > deadline {
                        anyhow::bail!(
                            "only {} decoded frames (wanted {min}), received={}, dropped={}, keyFrames={}",
                            stats.frames_decoded,
                            stats.frames_received,
                            stats.frames_dropped,
                            stats.key_frames_decoded,
                        );
                    }
                }
                Err(e) => {
                    if tokio::time::Instant::now() > deadline {
                        anyhow::bail!("getStats failed and timed out: {e}");
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn disconnect(&self) -> Result<()> {
        let remove_consumer: thirtyfour::WebElement = self
            .driver
            .query(thirtyfour::By::Id("remove-consumer"))
            .first()
            .await
            .map_err(|e| anyhow::anyhow!("remove-consumer not found: {e}"))?;
        remove_consumer
            .click()
            .await
            .map_err(|e| anyhow::anyhow!("click remove-consumer: {e}"))?;
        Ok(())
    }

    pub async fn quit(self) -> Result<()> {
        self.driver
            .quit()
            .await
            .map_err(|e| anyhow::anyhow!("WebDriver quit: {e}"))?;
        Ok(())
    }
}
