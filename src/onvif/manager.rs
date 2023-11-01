use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use async_std::stream::StreamExt;
use tracing::*;

use crate::video::types::VideoSourceType;
use crate::video::video_source_redirect::{VideoSourceRedirect, VideoSourceRedirectType};

use super::client::*;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Default::default();
}

#[derive(Debug)]
pub struct Manager {
    _server_thread_handle: std::thread::JoinHandle<()>,
}

impl Default for Manager {
    #[instrument(level = "trace")]
    fn default() -> Self {
        Self {
            _server_thread_handle: thread::Builder::new()
                .name("Onvif".to_string())
                .spawn(Manager::run_main_loop)
                .expect("Failed spawing Onvif thread"),
        }
    }
}

impl Manager {
    // Construct our manager, should be done inside main
    #[instrument(level = "debug")]
    pub fn init() {
        MANAGER.as_ref();
    }

    #[instrument(level = "debug", fields(endpoint))]
    fn run_main_loop() {
        tokio::runtime::Builder::new_multi_thread()
            .on_thread_start(|| debug!("Thread started"))
            .on_thread_stop(|| debug!("Thread stopped"))
            .thread_name_fn(|| {
                static ATOMIC_ID: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let id = ATOMIC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("Onvif-{id}")
            })
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed building a new tokio runtime")
            .block_on(Manager::discover_loop())
            .expect("Error starting Onvif server");
    }

    #[instrument(level = "debug")]
    async fn discover_loop() -> Result<()> {
        use futures::stream::StreamExt;
        use std::net::{IpAddr, Ipv4Addr};

        loop {
            info!("Discovering...");

            const MAX_CONCURRENT_JUMPERS: usize = 100;

            onvif::discovery::DiscoveryBuilder::default()
                .listen_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .duration(tokio::time::Duration::from_secs(5))
                .run()
                .await?
                .for_each_concurrent(MAX_CONCURRENT_JUMPERS, |device| async move {
                    info!("Device found: {device:#?}");

                    // TEST ONLY
                    // let cred = Some(onvif::soap::client::Credentials {
                    //     username: "admin".to_string(),
                    //     password: "admin".to_string(),
                    // });
                    let clients = match Clients::try_new(&Auth {
                        credentials: None,
                        url: device.urls.first().unwrap().to_string().into(),
                    })
                    .await
                    {
                        Ok(clients) => clients,
                        Err(error) => {
                            error!("Failed creating clients: {error:#?}");
                            return;
                        }
                    };

                    let stream_uris = &clients.get_stream_uris().await;
                    info!("stream_uris: {stream_uris:#?}");

                    let capabilities = &clients.get_capabilities().await;
                    info!("capabilities: {capabilities:#?}");

                    let device_information = &clients.get_device_information().await;
                    info!("device_information: {device_information:#?}");

                    let hostname = &clients.get_hostname().await;
                    info!("hostname: {hostname:#?}");

                    let service_capabilities = &clients.get_service_capabilities().await;
                    info!("service_capabilities: {service_capabilities:#?}");

                    let snapshot_uris = &clients.get_snapshot_uris().await;
                    info!("snapshot_uris: {snapshot_uris:#?}");

                    let status = &clients.get_status().await;
                    info!("status: {status:#?}");

                    let system_date_and_time = &clients.get_system_date_and_time().await;
                    info!("system_date_and_time: {system_date_and_time:#?}");

                    let analytics = &clients.get_analytics().await;
                    info!("analytics: {analytics:#?}");

                    // device.
                    // send to the video_source_redirect buffer
                })
                .await;
        }
    }
}
