use std::sync::{Arc, Mutex};

use anyhow::Result;
use tracing::*;

use crate::stream::types::CaptureConfiguration;
use crate::stream::{manager as stream_manager, types::StreamInformation};
use crate::video::types::VideoSourceType;
use crate::video::video_source_redirect::{VideoSourceRedirect, VideoSourceRedirectType};
use crate::video_stream::types::VideoAndStreamInformation;

use super::client::*;

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Default::default();
}

#[derive(Debug)]
pub struct Manager {
    _process: tokio::task::JoinHandle<Result<(), anyhow::Error>>,
}

impl Drop for Manager {
    fn drop(&mut self) {
        self._process.abort();
    }
}

impl Default for Manager {
    #[instrument(level = "trace")]
    fn default() -> Self {
        Self {
            _process: tokio::spawn(async move { Manager::discover_loop().await }),
        }
    }
}

impl Manager {
    // Construct our manager, should be done inside main
    #[instrument(level = "debug")]
    pub fn init() {
        MANAGER.as_ref();
    }

    #[instrument(level = "debug")]
    async fn discover_loop() -> Result<()> {
        use futures::stream::StreamExt;
        use std::net::{IpAddr, Ipv4Addr};

        loop {
            debug!("Discovering onvif...");

            const MAX_CONCURRENT_JUMPERS: usize = 100;

            onvif::discovery::DiscoveryBuilder::default()
                .listen_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .duration(tokio::time::Duration::from_secs(5))
                .run()
                .await?
                .for_each_concurrent(MAX_CONCURRENT_JUMPERS, |device| async move {
                    debug!("Device found: {device:#?}");

                    //TODO: We should add support to auth later
                    let credentials = None;
                    let clients = match Clients::try_new(&Auth {
                        credentials: credentials.clone(),
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

                    match clients.get_stream_uris().await {
                        Ok(stream_uris) => {
                            let mut url = stream_uris[0].clone();

                            let name = if let Ok(device) = &clients.get_device_information().await {
                                format!("{} - {} - {}", device.model, device.serial_number, url)
                            } else {
                                if let Some(name) = device.name {
                                    format!("{name} - {url}")
                                } else {
                                    format!("{url}")
                                }
                            };

                            if let Some(credentials) = credentials {
                                if url.set_username(&credentials.username).is_err() {
                                    error!("Failed setting username for {url}");
                                }
                                if url.set_password(Some(&credentials.password)).is_err() {
                                    error!("Failed setting password for {url}");
                                }
                            }
                            let video_source_redirect = VideoSourceRedirect {
                                name: name.clone(),
                                source: VideoSourceRedirectType::Redirect(
                                    stream_uris[0].to_string(),
                                ),
                            };

                            let video_and_stream = VideoAndStreamInformation {
                                name: name.clone(),
                                stream_information: StreamInformation {
                                    endpoints: vec![url],
                                    configuration: CaptureConfiguration::Redirect(
                                        Default::default(),
                                    ),
                                    extended_configuration: None,
                                },
                                video_source: VideoSourceType::Redirect(video_source_redirect),
                            };

                            if let Ok(streams) = stream_manager::streams().await {
                                for stream in streams {
                                    if let Err(error) =
                                        video_and_stream.conflicts_with(&stream.video_and_stream)
                                    {
                                        debug!("Stream {name} is already registered: {error}");
                                        return;
                                    }
                                }
                            }

                            if let Err(error) =
                                stream_manager::add_stream_and_start(video_and_stream).await
                            {
                                error!("Failed adding stream: {error:#?}");
                            }
                        }
                        Err(error) => {
                            error!("Failed getting stream uris: {error:#?}");
                        }
                    }
                })
                .await;
        }
    }
}
