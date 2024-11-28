use std::{collections::HashMap, net::Ipv4Addr, sync::Arc};

use anyhow::{anyhow, Context, Result};
use onvif::soap::client::Credentials;
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::*;
use url::Url;

use crate::video::{
    types::{Format, VideoSourceType},
    video_source_onvif::{VideoSourceOnvif, VideoSourceOnvifType},
};

use super::camera::*;

lazy_static! {
    static ref MANAGER: Arc<RwLock<Manager>> = Default::default();
}

pub struct Manager {
    mcontext: Arc<RwLock<ManagerContext>>,
    _task: tokio::task::JoinHandle<Result<(), anyhow::Error>>,
}

pub struct ManagerContext {
    cameras: HashMap<StreamURI, OnvifCamera>,
    /// Onvif devices discovered
    discovered_devices: HashMap<uuid::Uuid, OnvifDevice>,
}

type StreamURI = String;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OnvifDevice {
    pub uuid: uuid::Uuid,
    pub ip: Ipv4Addr,
    pub types: Vec<String>,
    pub hardware: Option<String>,
    pub name: Option<String>,
    pub urls: Vec<Url>,
}

impl TryFrom<onvif::discovery::Device> for OnvifDevice {
    type Error = anyhow::Error;

    fn try_from(device: onvif::discovery::Device) -> Result<Self, Self::Error> {
        Ok(Self {
            uuid: device_address_to_uuid(&device.address)?,
            ip: device
                .urls
                .first()
                .context("Device should have at least one URL")?
                .host_str()
                .context("Device URL should have a host")?
                .parse()?,
            types: device.types,
            hardware: device.hardware,
            name: device.name,
            urls: device.urls,
        })
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self._task.abort();
    }
}

impl Default for Manager {
    #[instrument(level = "debug")]
    fn default() -> Self {
        let mcontext = Arc::new(RwLock::new(ManagerContext {
            cameras: HashMap::new(),
            discovered_devices: HashMap::new(),
        }));

        let mcontext_clone = mcontext.clone();
        let _task = tokio::spawn(async { Manager::discover_loop(mcontext_clone).await });

        Self { mcontext, _task }
    }
}

impl Manager {
    // Construct our manager, should be done inside main
    #[instrument(level = "debug")]
    pub async fn init() {
        MANAGER.as_ref();

        let manager = MANAGER.write().await;

        let mut mcontext = manager.mcontext.write().await;

        // TODO: fill MANAGER.context.credentials with credentials passed by ENV and CLI
        // It can be in the form of "<USER>:<PWD>@<IP>", but we need to escape special characters need to.
        let _ = mcontext.credentials.insert(
            "192.168.0.168".to_string(),
            Arc::new(RwLock::new(Credentials {
                username: "admin".to_string(),
                password: "12345".to_string(),
            })),
        );
    }

    #[instrument(level = "debug")]
    pub async fn onvif_devices() -> Vec<OnvifDevice> {
        let mcontext = MANAGER.read().await.mcontext.clone();
        let mcontext = mcontext.read().await;

        mcontext.discovered_devices.values().cloned().collect()
    }

    async fn discover_loop(context: Arc<RwLock<ManagerContext>>) -> Result<()> {
        use futures::stream::StreamExt;
        use std::net::{IpAddr, Ipv4Addr};

        loop {
            trace!("Discovering onvif...");

            const MAX_CONCURRENT_JUMPERS: usize = 100;

            onvif::discovery::DiscoveryBuilder::default()
                .listen_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .duration(tokio::time::Duration::from_secs(20))
                .run()
                .await?
                .for_each_concurrent(MAX_CONCURRENT_JUMPERS, |device| {
                    let context = context.clone();

                    async move {
                        trace!("Device found: {device:#?}");

                        for url in device.urls {
                            let host = url
                                .host()
                                .map(|host| host.to_string())
                                .unwrap_or(url.to_string());

                            let credentials = if let Some(credentials) =
                                context.read().await.credentials.get(&host)
                            {
                                Some(credentials.read().await.clone())
                            } else {
                                None
                            };

                            trace!("Device {host}. Using credentials: {credentials:?}");

                            let camera = match OnvifCamera::try_new(&Auth {
                                credentials: credentials.clone(),
                                url: url.clone(),
                            })
                            .await
                            {
                                Ok(camera) => camera,
                                Err(error) => {
                                    error!(host, "Failed creating camera: {error:?}");
                                    return;
                                }
                            };

                            let Some(streams_informations) = &camera.streams_information else {
                                error!(host, "Failed getting stream information");
                                continue;
                            };

                            trace!(host, "Found streams {streams_informations:?}");

                            let mut context = context.write().await;
                            for stream_information in streams_informations {
                                context
                                    .cameras
                                    .entry(stream_information.stream_uri.to_string())
                                    .and_modify(|old_camera| *old_camera = camera.clone())
                                    .or_insert_with(|| {
                                        debug!(host, "New stream inserted: {stream_information:?}");

                                        camera.clone()
                                    });
                            }
                        }
                    }
                })
                .await;
        }
    }

    #[instrument(level = "debug")]
    pub async fn get_formats(stream_uri: &StreamURI) -> Result<Vec<Format>> {
        let mcontext = MANAGER.read().await.mcontext.clone();
        let mcontext = mcontext.read().await;

        let camera = mcontext
            .cameras
            .get(stream_uri)
            .context("Camera not found")?;

        let Some(streams_information) = &camera.streams_information else {
            return Err(anyhow!("Failed getting stream information"));
        };

        let stream_information = streams_information
            .iter()
            .find(|&stream_information| &stream_information.stream_uri.to_string() == stream_uri)
            .context("Camera not found")?;

        Ok(vec![stream_information.format.clone()])
    }

    #[instrument(level = "debug")]
    pub async fn streams_available() -> Vec<VideoSourceType> {
        let mcontext = MANAGER.read().await.mcontext.clone();
        let mcontext = mcontext.read().await;

        mcontext
            .cameras
            .keys()
            .map(|stream_uri| {
                VideoSourceType::Onvif(VideoSourceOnvif {
                    name: format!("{stream_uri}"),
                    source: VideoSourceOnvifType::Onvif(stream_uri.clone()),
                })
            })
            .collect::<Vec<VideoSourceType>>()
    }
}

/// Address must be something like `urn:uuid:bc071801-c50f-8301-ac36-bc071801c50f`.
/// Read 7 Device discovery from [ONVIF-Core-Specification](https://www.onvif.org/specs/core/ONVIF-Core-Specification-v1612a.pdf)
#[instrument(level = "debug")]
fn device_address_to_uuid(device_address: &str) -> Result<uuid::Uuid> {
    device_address
        .split(':')
        .last()
        .context("Failed to parse device address into a UUID")?
        .parse()
        .map_err(anyhow::Error::msg)
}
