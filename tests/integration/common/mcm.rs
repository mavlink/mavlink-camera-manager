use std::{
    net::{TcpListener, UdpSocket},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};

pub struct McmProcess {
    child: Child,
    pub rest_port: u16,
    pub signalling_port: u16,
    pub rtsp_port: u16,
    pub zenoh_port: Option<u16>,
    _settings_dir: tempfile::TempDir,
}

const START_RETRIES: u32 = 3;

impl McmProcess {
    pub async fn start() -> Result<Self> {
        Self::start_with_options(None).await
    }

    /// Spawn an MCM instance with zenoh enabled in peer mode.
    pub async fn start_with_zenoh() -> Result<Self> {
        let mut last_err = None;
        for attempt in 0..START_RETRIES {
            match Self::try_start_inner(None, true).await {
                Ok(mcm) => return Ok(mcm),
                Err(e) => {
                    eprintln!(
                        "McmProcess start_with_zenoh attempt {}/{START_RETRIES} failed: {e:#}",
                        attempt + 1
                    );
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("McmProcess::start_with_zenoh failed")))
    }

    /// Spawn an MCM instance with freshly allocated ephemeral ports.
    ///
    /// Retries up to [`START_RETRIES`] times to handle the TOCTOU race
    /// between port allocation (bind-to-:0-then-drop) and the MCM binary
    /// actually binding those ports.
    pub async fn start_with_options(mavlink_endpoint: Option<&str>) -> Result<Self> {
        let mut last_err = None;
        for attempt in 0..START_RETRIES {
            match Self::try_start_inner(mavlink_endpoint, false).await {
                Ok(mcm) => return Ok(mcm),
                Err(e) => {
                    eprintln!(
                        "McmProcess start attempt {}/{START_RETRIES} failed: {e:#}",
                        attempt + 1
                    );
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("McmProcess::start failed")))
    }

    /// Build a `zenoh::Config` that connects to this MCM instance's zenoh
    /// peer port. Panics if zenoh was not enabled.
    pub fn zenoh_config(&self) -> zenoh::Config {
        let port = self
            .zenoh_port
            .expect("zenoh_config() called but MCM was started without zenoh");
        let mut config = zenoh::Config::default();
        config
            .insert_json5("mode", r#""peer""#)
            .expect("insert mode");
        config
            .insert_json5("connect/endpoints", &format!(r#"["tcp/127.0.0.1:{port}"]"#))
            .expect("insert connect endpoints");
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .expect("insert scouting");
        config
    }

    async fn try_start_inner(mavlink_endpoint: Option<&str>, enable_zenoh: bool) -> Result<Self> {
        let tcp_count = if enable_zenoh { 4 } else { 3 };
        let ports = allocate_ports(tcp_count)?;
        let rest_port = ports[0];
        let signalling_port = ports[1];
        let rtsp_port = ports[2];
        let zenoh_port = if enable_zenoh { Some(ports[3]) } else { None };

        let binary = mcm_binary_path();

        let settings_dir = tempfile::tempdir().context("creating temp settings dir")?;
        let settings_file = settings_dir.path().join("settings.json");

        let mavlink_fallback;
        let mavlink_arg = match mavlink_endpoint {
            Some(ep) => ep,
            None => {
                let mav_port = allocate_udp_ports(1)?[0];
                mavlink_fallback = format!("udpin:127.0.0.1:{mav_port}");
                &mavlink_fallback
            }
        };

        let log_path = settings_dir.path().join("logs");

        let mut cmd = Command::new(&binary);
        cmd.args([
            "--reset",
            "--verbose",
            "--rest-server",
            &format!("127.0.0.1:{rest_port}"),
            "--signalling-server",
            &format!("ws://127.0.0.1:{signalling_port}"),
            "--rtsp-port",
            &rtsp_port.to_string(),
            "--settings-file",
            settings_file.to_str().unwrap(),
            "--log-path",
            log_path.to_str().unwrap(),
            "--mavlink",
            mavlink_arg,
            "--disable-onvif",
        ]);

        if let Some(port) = zenoh_port {
            let zenoh_config_path = settings_dir.path().join("zenoh_config.json5");
            std::fs::write(
                &zenoh_config_path,
                format!(
                    r#"{{
  "mode": "peer",
  "listen": {{ "endpoints": ["tcp/127.0.0.1:{port}"] }},
  "scouting": {{ "multicast": {{ "enabled": false }} }}
}}"#
                ),
            )
            .context("writing zenoh config")?;
            cmd.args([
                "--zenoh",
                "--zenoh-config-file",
                zenoh_config_path.to_str().unwrap(),
            ]);
        }

        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let child = cmd
            .spawn()
            .with_context(|| format!("spawning MCM binary at {}", binary.display()))?;

        let mcm = Self {
            child,
            rest_port,
            signalling_port,
            rtsp_port,
            zenoh_port,
            _settings_dir: settings_dir,
        };

        mcm.wait_ready(Duration::from_secs(30)).await?;
        Ok(mcm)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn rest_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.rest_port)
    }

    pub fn signalling_url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.signalling_port)
    }

    pub fn rtsp_url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("rtsp://127.0.0.1:{}/{path}", self.rtsp_port)
    }

    pub async fn wait_for_rtsp_ready(&self, path: &str, timeout: Duration) {
        let addr = self
            .rtsp_url(path)
            .trim_start_matches("rtsp://")
            .split('/')
            .next()
            .unwrap_or("127.0.0.1:8554")
            .to_string();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(Ok(_)) = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "RTSP server at {addr} not accepting TCP within {timeout:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub fn stop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() >= deadline => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return,
            }
        }
        eprintln!(
            "[McmProcess] child {} did not exit after SIGTERM, sending SIGKILL",
            self.child.id()
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let url = format!("{}/info", self.rest_url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::time::Instant::now() > deadline {
                anyhow::bail!(
                    "MCM did not become ready within {}s (GET {url})",
                    timeout.as_secs()
                );
            }
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    }
}

impl Drop for McmProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn allocate_ports(n: u8) -> Result<Vec<u16>> {
    let listeners: Vec<TcpListener> = (0..n)
        .map(|_| TcpListener::bind("127.0.0.1:0"))
        .collect::<std::io::Result<_>>()?;
    let ports = listeners
        .iter()
        .map(|l| l.local_addr().map(|a| a.port()))
        .collect::<std::io::Result<_>>()?;
    drop(listeners);
    Ok(ports)
}

pub fn allocate_udp_ports(n: u8) -> Result<Vec<u16>> {
    let sockets: Vec<UdpSocket> = (0..n)
        .map(|_| UdpSocket::bind("127.0.0.1:0"))
        .collect::<std::io::Result<_>>()?;
    let ports = sockets
        .iter()
        .map(|s| s.local_addr().map(|a| a.port()))
        .collect::<std::io::Result<_>>()?;
    drop(sockets);
    Ok(ports)
}

fn mcm_binary_path() -> PathBuf {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    manifest_dir
        .join("target")
        .join(profile)
        .join("mavlink-camera-manager")
}
