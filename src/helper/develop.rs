use std::collections::HashMap;

use crate::cli;
use crate::helper;
use anyhow::{anyhow, Result};
use thirtyfour::prelude::*;
use tokio::runtime::Runtime;
use tokio::sync::RwLock;
use tracing::*;

async fn prepare() -> Result<WebDriver> {
    let mut caps = DesiredCapabilities::chrome();
    caps.set_headless()?;

    let chromiumdriver_port = cli::manager::enable_webrtc_task_test().unwrap();
    let webdriver =
        WebDriver::new(&format!("http://localhost:{chromiumdriver_port}"), caps).await?;

    let frontend_address = cli::manager::server_address();
    let webrtc_frontend_url = format!("http://{frontend_address}/webrtc/index.html");
    webdriver.goto(webrtc_frontend_url).await?;

    // Wait for the system to stabilize
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    Ok(webdriver)
}

fn get_difference_map<K, V>(map1: &HashMap<K, V>, map2: &HashMap<K, V>) -> HashMap<K, V>
where
    K: std::hash::Hash + Eq + Copy,
    V: Clone,
{
    map1.iter()
        .filter(|(k, _)| !map2.contains_key(k))
        .map(|(k, v)| (*k, v.clone()))
        .collect()
}

fn has_common_entries<K, V>(map1: &HashMap<K, V>, map2: &HashMap<K, V>) -> bool
where
    K: std::hash::Hash + Eq,
    V: PartialEq,
{
    map2.iter()
        .any(|(key, value)| map1.get(key).map_or(false, |v| v == value))
}

async fn task(session_cycles: i32) -> Result<()> {
    let webdriver = prepare().await?;

    // Configurations
    let sessions_per_consumer = 5;

    // Start of test
    let initial_tasks = helper::threads::process_tasks();
    let tasks_last_cycle = RwLock::new(initial_tasks.clone());
    let current_tasks = RwLock::new(HashMap::default());
    let new_tasks_since_start = RwLock::new(HashMap::default());
    let new_tasks_since_last_cycle = RwLock::new(HashMap::default());
    let tasks_alive_from_last_cycle = RwLock::new(HashMap::default());

    info!(
        "Started webrtc test. Number of tasks: {}",
        initial_tasks.len()
    );

    for current_cycle in 0..=session_cycles {
        let add_consumer_button = webdriver.query(By::Id("add-consumer")).first().await?;
        add_consumer_button.click().await?;

        // Add all sessions
        let add_session_button = webdriver.query(By::Id("add-session")).first().await?;
        for _ in 0..sessions_per_consumer {
            add_session_button.click().await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        // Wait for all statuses be "Playing"
        tokio::time::timeout(tokio::time::Duration::from_secs(30), {
            async {
                loop {
                    let elements = match webdriver
                        .query(By::Id("session-status"))
                        .with_text("Status: Playing")
                        .all()
                        .await
                    {
                        Ok(elements) => elements,
                        Err(error) => break Err(error),
                    };

                    if elements.len().eq(&sessions_per_consumer) {
                        break Ok(());
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
        })
        .await??;

        info!("All sessions are Playing");

        // Remove consumer, also removing all sessions
        let remove_consumer_button = webdriver.query(By::Id("remove-consumer")).first().await?;
        remove_consumer_button.click().await?;

        info!("Consumer removed, waiting for tasks to finish...");

        *tasks_alive_from_last_cycle.write().await =
            new_tasks_since_last_cycle.read().await.clone();

        // Wait for tasks to die
        let wait_for_tasks_to_die = async {
            let mut current_task = current_tasks.write().await;
            let mut new_tasks_since_start = new_tasks_since_start.write().await;
            let mut new_tasks_since_last_cycle = new_tasks_since_last_cycle.write().await;
            let tasks_last_cycle = tasks_last_cycle.read().await;
            let tasks_alive_from_last_cycle = tasks_alive_from_last_cycle.read().await;

            loop {
                *current_task = helper::threads::process_tasks();
                *new_tasks_since_start = get_difference_map(&current_task, &initial_tasks);
                *new_tasks_since_last_cycle = get_difference_map(&current_task, &tasks_last_cycle);

                let all_tasks_alive_from_last_cycle_are_dead =
                    has_common_entries(&current_task, &tasks_alive_from_last_cycle);

                let no_key_tasks_leaked_since_last_cycle =
                    !new_tasks_since_last_cycle.values().any(|task_name| {
                        let task_name = task_name.to_lowercase();

                        task_name.starts_with("webrtcbin")
                            || task_name.starts_with("nicesrc")
                            || task_name.starts_with("rtpsession")
                    });

                if no_key_tasks_leaked_since_last_cycle && all_tasks_alive_from_last_cycle_are_dead
                {
                    break;
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        };

        if tokio::time::timeout(tokio::time::Duration::from_secs(30), wait_for_tasks_to_die)
            .await
            .is_err()
        {
            // Ignore first cycle
            if current_cycle > 0 {
                return Err(anyhow!(
                "Thread leak detected on cycle {current_cycle}:\n{new_tasks_since_last_cycle:#?}\n{tasks_alive_from_last_cycle:#?}"
            ));
            }
        };

        let number_of_tasks = current_tasks.read().await.len();
        let number_of_new_tasks_since_start = new_tasks_since_start.read().await.len();
        let number_of_new_tasks_since_last_cycle = new_tasks_since_last_cycle.read().await.len();
        let number_of_tasks_alive_from_last_cycle = tasks_alive_from_last_cycle.read().await.len();

        *tasks_last_cycle.write().await = helper::threads::process_tasks();

        info!("Successful cycles: {current_cycle}/{session_cycles}");
        info!("Current tasks: {number_of_tasks}");
        info!("New tasks since start: {number_of_new_tasks_since_start}");
        info!("New tasks since last cycle: {number_of_new_tasks_since_last_cycle}");
        info!("Tasks alive since last cycle: {number_of_tasks_alive_from_last_cycle}");

        if number_of_new_tasks_since_last_cycle > 0 || number_of_tasks_alive_from_last_cycle > 0 {
            info!("The following tasks were created since last cycle:\n{new_tasks_since_last_cycle:#?}");
            info!("The following tasks were alive since last cycle:\n{tasks_alive_from_last_cycle:#?}")
        }
    }

    Ok(())
}

pub fn start_check_tasks_on_webrtc_reconnects() {
    std::thread::spawn(move || {
        let rt = Runtime::new().unwrap();

        info!("Starting WebRTC test...");
        if let Err(error) = rt.block_on(task(5)) {
            error!("WebRTC test failed: {error:?}");
            std::process::exit(-1);
        }

        info!("WebRTC test passed!");
        std::process::exit(0);
    });
}
