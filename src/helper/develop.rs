use crate::cli;
use crate::helper;
use anyhow::Result;
use core::time::Duration;
use std::thread;
use thirtyfour::prelude::*;
use tokio::runtime::Runtime;
use tracing::*;

async fn task(mut counter: i32) -> Result<()> {
    info!("Started webrtc test..");

    let mut caps = DesiredCapabilities::chrome();
    let _ = caps.set_headless();

    let port = cli::manager::enable_webrtc_task_test().unwrap();
    let driver = WebDriver::new(&format!("http://localhost:{}", port), caps)
        .await
        .expect("Failed to create web driver.");

    driver
        .goto("http://0.0.0.0:6020/webrtc/index.html")
        .await
        .expect("Failed to connect to local webrtc page.");

    loop {
        for button in ["add-consumer", "add-session", "remove-all-consumers"] {
            driver
                .query(By::Id(button))
                .wait(Duration::from_secs(10), Duration::from_millis(100))
                .first()
                .await?
                .click()
                .await?;
        }

        counter += 1;

        info!("Restarted webrtc {} times", counter);
        let number_of_tasks = helper::threads::process_task_counter();
        if number_of_tasks > 100 {
            error!("Thead leak detected: {number_of_tasks}");
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
