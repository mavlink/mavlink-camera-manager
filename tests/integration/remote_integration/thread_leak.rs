use std::{collections::HashMap, time::Duration};

use sysinfo::{PidExt, ProcessExt, System, SystemExt};

use super::*;

const SESSIONS_PER_CYCLE: usize = 5;
const CYCLES: i32 = 5;
const THREAD_DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

const INFRA_THREAD_PREFIXES: &[&str] = &[
    "tokio-runtime-w",
    "pool-",
    "actix-server",
    "actix-rt",
    "[pango]",
    "gmain",
    "gdbus",
    "dconf worker",
    "GstSystemClock",
];

fn is_infrastructure_thread(name: &str) -> bool {
    INFRA_THREAD_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn list_threads(pid: u32) -> HashMap<u32, String> {
    let mut system = System::new_all();
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);
    system.refresh_process(sysinfo_pid);

    #[cfg(target_os = "linux")]
    {
        system
            .process(sysinfo_pid)
            .map(|proc_| {
                proc_
                    .tasks
                    .iter()
                    .map(|(tid, task)| (tid.as_u32(), task.name().to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = system;
        HashMap::new()
    }
}

fn get_new_threads(
    current: &HashMap<u32, String>,
    baseline: &HashMap<u32, String>,
) -> HashMap<u32, String> {
    current
        .iter()
        .filter(|(k, _)| !baseline.contains_key(k))
        .map(|(k, v)| (*k, v.clone()))
        .collect()
}

fn filter_suspicious(threads: HashMap<u32, String>) -> HashMap<u32, String> {
    threads
        .into_iter()
        .filter(|(_, name)| !is_infrastructure_thread(name))
        .collect()
}

/// Stress-test WebRTC session teardown to detect thread leaks.
///
/// Cycle 0 is a warmup: it exercises all code paths so that lazily-initialized
/// system threads (pango, GStreamer queues, etc.) have already been created
/// before the real measurement starts. The post-warmup thread snapshot becomes
/// the stable baseline. Cycles 1..=CYCLES then compare against that baseline
/// and fail if any non-infrastructure threads persist after teardown.
#[tokio::test]
async fn test_webrtc_thread_leak() {
    skip_unless!(SourceTag::Fake);

    let env = TestEnv::setup().await;
    let mcm_pid = env.mcm_pid();
    let c = env.client();

    eprintln!(
        "WebRTC thread leak test: MCM PID={mcm_pid}, initial threads={}",
        list_threads(mcm_pid).len()
    );

    // -- Warmup cycle (cycle 0) --
    {
        let mut clients = Vec::with_capacity(SESSIONS_PER_CYCLE);
        for _ in 0..SESSIONS_PER_CYCLE {
            clients.push(
                webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await,
            );
        }
        for client in &clients {
            client
                .wait_for_frames(5, cold_timeout())
                .await
                .unwrap_or_else(|e| panic!("warmup: client failed to receive frames: {e}"));
        }
        drop(clients);
        ensure_idle(&c).await;
    }

    // Wait for thread count to stabilize before taking baseline
    let stable_baseline = tokio::time::timeout(THREAD_DRAIN_TIMEOUT, async {
        let mut prev = list_threads(mcm_pid);
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let now = list_threads(mcm_pid);
            if now.len() == prev.len() {
                return now;
            }
            eprintln!(
                "warmup drain: thread count changed {} -> {}, waiting...",
                prev.len(),
                now.len()
            );
            prev = now;
        }
    })
    .await
    .expect("Timed out waiting for threads to stabilize after warmup");

    eprintln!(
        "Stable baseline after warmup: {} threads",
        stable_baseline.len()
    );

    for cycle in 1..=CYCLES {
        let mut clients = Vec::with_capacity(SESSIONS_PER_CYCLE);
        for _ in 0..SESSIONS_PER_CYCLE {
            clients.push(
                webrtc_connect_with_retry(&env.signalling_url, env.ice_filter.as_deref()).await,
            );
        }

        for client in &clients {
            client
                .wait_for_frames(5, cold_timeout())
                .await
                .unwrap_or_else(|e| panic!("cycle {cycle}: client failed to receive frames: {e}"));
        }

        drop(clients);
        ensure_idle(&c).await;

        let surviving = tokio::time::timeout(THREAD_DRAIN_TIMEOUT, async {
            loop {
                let current = list_threads(mcm_pid);
                let leaked = filter_suspicious(get_new_threads(&current, &stable_baseline));
                if leaked.is_empty() {
                    return leaked;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await;

        let leaked = match surviving {
            Ok(threads) => threads,
            Err(_) => {
                let current = list_threads(mcm_pid);
                filter_suspicious(get_new_threads(&current, &stable_baseline))
            }
        };

        eprintln!("cycle {cycle}/{CYCLES}: leaked={}", leaked.len());

        if !leaked.is_empty() {
            eprintln!("surviving threads: {leaked:#?}");
            panic!(
                "Thread leak detected on cycle {cycle}: {} threads survived: {leaked:#?}",
                leaked.len()
            );
        }
    }

    eprintln!("WebRTC thread leak test passed!");
}
