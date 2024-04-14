use crate::cli;
use crate::helper;
use anyhow::Result;
use core::time::Duration;
use std::thread;
use thirtyfour::prelude::*;
use tokio::runtime::Runtime;
use tracing::*;

async fn task(mut counter: i32) -> Result<()> {
    // Wait for the system to stabilize
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    let initial_number_of_tasks = helper::threads::process_task_counter();

    info!("Started webrtc test. Number of tasks: {initial_number_of_tasks}");

    let mut caps = DesiredCapabilities::chrome();
    caps.set_headless()?;

    let port = cli::manager::enable_webrtc_task_test().unwrap();
    let driver = WebDriver::new(&format!("http://localhost:{}", port), caps)
        .await
        .unwrap_or_else(|_| {
            error!("Failed to connect with WebDriver.");
            std::process::exit(-1)
        });

    driver
        .goto("http://0.0.0.0:6020/webrtc/index.html")
        .await
        .expect("Failed to connect to local webrtc page.");

    let mut number_of_tasks_last_cycle = initial_number_of_tasks;
    loop {
        for button_id in [
            "add-consumer",
            "add-session",
            "remove-session",
            "remove-consumer",
        ] {
            info!("Looking for html element id: {button_id:?}");

            let button = driver
                .query(By::Id(button_id))
                .and_enabled()
                .and_clickable()
                .and_displayed()
                .wait(Duration::from_secs(1), Duration::from_millis(100))
                .first()
                .await
                .inspect_err(|error| {
                    error!("Failed to query button id {button_id:?}: {error:?}")
                })?;

            button.click().await.inspect_err(|error| {
                error!("Failed to click button id {button_id:?}: {error:?}")
            })?;

            info!("Button id {button_id:?}: clicked");

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }

        counter += 1;

        let number_of_tasks = helper::threads::process_task_counter();
        let number_of_new_tasks_since_start =
            number_of_tasks as i32 - initial_number_of_tasks as i32;
        let number_of_new_tasks_since_last_cycle =
            number_of_tasks as i32 - number_of_tasks_last_cycle as i32;
        number_of_tasks_last_cycle = number_of_tasks;

        info!("Recycled webrtc {counter} times");
        info!("New tasks since start: {number_of_new_tasks_since_start}");
        info!("New tasks since last cycle: {number_of_new_tasks_since_last_cycle}");

        if counter > 1 && number_of_new_tasks_since_last_cycle > 1 {
            error!("Thead leak detected!");
            std::process::exit(-1);
        }
    }
}

pub fn start_check_tasks_on_webrtc_reconnects() {
    let counter = 0;
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            loop {
                if let Err(error) = task(counter).await {
                    error!("WebRTC Checker Task failed: {error:#?}");
                    std::process::exit(-1);
                }
            }
        });
        error!("Webrtc test failed internally.");
        std::process::exit(-1);
    });
}
