use mavlink_camera_manager::{
    cli, controls, helper, logger, mavlink, server, settings, stream, zenoh,
};

use tracing::*;

fn main() -> Result<(), std::io::Error> {
    helper::threads::lower_thread_priority();

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(10)
        .enable_all()
        .on_thread_start(helper::threads::lower_thread_priority)
        .build()
        .expect("Failed to create Tokio runtime")
        .block_on(async_main())
}

async fn async_main() -> Result<(), std::io::Error> {
    // CLI should be started before logger to allow control over verbosity
    cli::manager::init();
    // Logger should start before everything else to register any log information
    logger::manager::init();
    // Settings should start before everybody else to ensure that the CLI are stored
    settings::manager::init(Some(&cli::manager::settings_file())).await;

    stream::gst::utils::check_all_plugins().unwrap();

    mavlink::manager::Manager::init();

    stream::manager::init();
    settings::manager::set_mavlink_endpoint(&cli::manager::mavlink_connection_string());

    // Onvif should start after the stream manager
    controls::onvif::manager::Manager::init().await;

    if cli::manager::enable_zenoh() {
        zenoh::init().await.unwrap();
    }

    if cli::manager::enable_thread_counter() {
        helper::threads::start_thread_counter_thread();
    }

    #[cfg(feature = "webrtc-test")]
    if cli::manager::enable_webrtc_task_test().is_some() {
        helper::develop::start_check_tasks_on_webrtc_reconnects();
    }

    let _signalling_server = stream::webrtc::signalling_server::SignallingServer::default();

    if let Err(error) = stream::manager::start_default().await {
        error!("Failed to start default streams. Reason: {error:?}")
    }

    server::manager::run(&cli::manager::server_address()).await?;

    Ok(())
}
