use std::{
    collections::HashMap,
    net::{TcpListener, UdpSocket},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{LazyLock, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};

/// Bind-to-`:0`-then-drop is racy under parallel nextest: another worker can
/// bind the same number before this test's MCM (or `TestRtspServer`) does.
/// Directory leases stay alive until the test process exits. `create_dir` is
/// atomic even when `flock` is unreliable on overlayfs in CI containers.
static TCP_PORT_LEASES: LazyLock<Mutex<HashMap<u16, PortLease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static UDP_PORT_LEASES: LazyLock<Mutex<HashMap<u16, PortLease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct PortLease {
    directory: PathBuf,
}

impl Drop for PortLease {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.directory) {
            Ok(()) => {}
            // Best-effort: a leftover directory is stolen once its process is dead.
            Err(_) => {}
        }
    }
}

const PORT_CLAIM_ATTEMPT_LIMIT: u32 = 10_000;

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
        Self::start_with_retry(None, true).await
    }

    /// Spawn an MCM instance with freshly allocated ephemeral ports.
    ///
    /// Retries up to [`START_RETRIES`] times if MCM fails to come up (for
    /// example REST still colliding). Port numbers themselves are leased
    /// until this process exits so parallel nextest workers cannot reuse them.
    pub async fn start_with_options(mavlink_endpoint: Option<&str>) -> Result<Self> {
        Self::start_with_retry(mavlink_endpoint, false).await
    }

    async fn start_with_retry(mavlink_endpoint: Option<&str>, zenoh: bool) -> Result<Self> {
        crate::common::init_tracing();
        let mut last_err = None;
        for attempt in 0..START_RETRIES {
            match Self::try_start_inner(mavlink_endpoint, zenoh).await {
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
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("McmProcess start failed")))
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

    pub async fn wait_for_rtsp_ready(&self, path: &str, timeout: Duration) -> Result<()> {
        super::poll::wait_for_rtsp_tcp(&self.rtsp_url(path), timeout).await
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

/// Allocate `count` TCP ports and lease them until this process exits.
pub fn allocate_ports(count: u8) -> Result<Vec<u16>> {
    let mut ports = Vec::with_capacity(count as usize);
    let mut attempts = 0u32;
    while (ports.len() as u8) < count {
        attempts += 1;
        if attempts > PORT_CLAIM_ATTEMPT_LIMIT {
            anyhow::bail!("could not allocate {count} exclusive TCP ports");
        }
        let listener = TcpListener::bind("0.0.0.0:0").context("binding ephemeral TCP port")?;
        let port = listener
            .local_addr()
            .context("reading bound TCP address")?
            .port();
        if try_claim_port(port, "tcp", &TCP_PORT_LEASES) {
            ports.push(port);
        }
    }
    Ok(ports)
}

/// Allocate `count` UDP ports and lease them until this process exits.
pub fn allocate_udp_ports(count: u8) -> Result<Vec<u16>> {
    let mut ports = Vec::with_capacity(count as usize);
    let mut attempts = 0u32;
    while (ports.len() as u8) < count {
        attempts += 1;
        if attempts > PORT_CLAIM_ATTEMPT_LIMIT {
            anyhow::bail!("could not allocate {count} exclusive UDP ports");
        }
        let socket = UdpSocket::bind("127.0.0.1:0").context("binding ephemeral UDP port")?;
        let port = socket
            .local_addr()
            .context("reading bound UDP address")?
            .port();
        if try_claim_port(port, "udp", &UDP_PORT_LEASES) {
            ports.push(port);
        }
    }
    Ok(ports)
}

fn try_claim_port(
    port: u16,
    protocol: &str,
    leases: &'static Mutex<HashMap<u16, PortLease>>,
) -> bool {
    let mut leases = leases
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if leases.contains_key(&port) {
        return false;
    }

    let directory = std::env::temp_dir().join(format!(
        "mavlink-camera-manager-integration-{protocol}-{port}.lock"
    ));
    if !create_lease_directory(&directory) {
        return false;
    }

    leases.insert(port, PortLease { directory });
    true
}

fn create_lease_directory(directory: &PathBuf) -> bool {
    match std::fs::create_dir(directory) {
        Ok(()) => write_lease_process_id(directory),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if lease_owner_is_alive(directory) {
                return false;
            }
            match std::fs::remove_dir_all(directory) {
                Ok(()) => {}
                Err(_) => return false,
            }
            match std::fs::create_dir(directory) {
                Ok(()) => write_lease_process_id(directory),
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

fn write_lease_process_id(directory: &PathBuf) -> bool {
    std::fs::write(directory.join("process_id"), std::process::id().to_string()).is_ok()
}

fn lease_owner_is_alive(directory: &PathBuf) -> bool {
    let Ok(process_id_text) = std::fs::read_to_string(directory.join("process_id")) else {
        return false;
    };
    let Ok(process_id) = process_id_text.trim().parse::<u32>() else {
        return false;
    };
    process_is_alive(process_id)
}

fn process_is_alive(process_id: u32) -> bool {
    #[cfg(unix)]
    {
        // Safety: signal 0 only checks whether the process exists.
        unsafe { libc::kill(process_id as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _process_id = process_id;
        true
    }
}

fn mcm_binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mavlink-camera-manager"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_ports_does_not_reuse_a_leased_tcp_port() {
        let first = allocate_ports(8).unwrap();
        let second = allocate_ports(8).unwrap();
        for port in &first {
            assert!(
                !second.contains(port),
                "leased TCP port {port} was issued twice"
            );
        }
    }
}
