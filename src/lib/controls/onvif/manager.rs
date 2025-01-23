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
    /// Onvif Cameras
    cameras: HashMap<StreamURI, OnvifCamera>,
    /// Onvif devices discovered
    discovered_devices: HashMap<uuid::Uuid, OnvifDevice>,
    /// Credentials can be either added in runtime, or loaded from settings
    credentials: HashMap<uuid::Uuid, Credentials>,
    /// Credentials provided via url, such as those provided via ENV or CLI
    url_credentials: HashMap<Ipv4Addr, Credentials>,
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
        let url_credentials = crate::cli::manager::onvif_auth();

        let mcontext = Arc::new(RwLock::new(ManagerContext {
            cameras: HashMap::new(),
            discovered_devices: HashMap::new(),
            credentials: HashMap::new(),
            url_credentials,
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
    }

    #[instrument(level = "debug", skip_all)]
    pub async fn register_credentials(
        device_uuid: uuid::Uuid,
        credentials: Option<Credentials>,
    ) -> Result<()> {
        let mcontext = MANAGER.read().await.mcontext.clone();
        let mut mcontext = mcontext.write().await;

        match credentials {
            Some(credentials) => {
                let _ = mcontext.credentials.insert(device_uuid, credentials);
            }
            None => {
                let _ = mcontext.credentials.remove(&device_uuid);
            }
        }

        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    /// Expect onvif://<user>:<password>@<ip>:<port>/<path>, where path and port are ignored, and all the rest is mandatory.
    pub fn credentials_from_url(url: &url::Url) -> Result<(Ipv4Addr, Credentials)> {
        if url.scheme().ne("onvif") {
            return Err(anyhow!("Scheme must be `onvif`"));
        }

        let host = url
            .host_str()
            .context("Host must be provided")?
            .parse::<Ipv4Addr>()?;

        let password = url
            .password()
            .context("Password must be provided")?
            .to_string();

        Ok((
            host,
            Credentials {
                username: url.username().to_string(),
                password,
            },
        ))
    }

    #[instrument(level = "debug")]
    pub async fn onvif_devices() -> Vec<OnvifDevice> {
        let mcontext = MANAGER.read().await.mcontext.clone();
        let mcontext = mcontext.read().await;

        mcontext.discovered_devices.values().cloned().collect()
    }

    #[instrument(level = "debug", skip(mcontext))]
    async fn discover_loop(mcontext: Arc<RwLock<ManagerContext>>) -> Result<()> {
        use futures::stream::StreamExt;
        use std::net::{IpAddr, Ipv4Addr};

        let scan_duration = tokio::time::Duration::from_secs(20);

        loop {
            trace!("Discovering onvif...");

            const MAX_CONCURRENT_JUMPERS: usize = 100;

            onvif::discovery::DiscoveryBuilder::default()
                .listen_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .duration(scan_duration)
                .run()
                .await?
                .for_each_concurrent(MAX_CONCURRENT_JUMPERS, |device| {
                    let context = mcontext.clone();

                    async move {
                        let device: OnvifDevice = match device.try_into() {
                            Ok(onvif_device) => onvif_device,
                            Err(error) => {
                                error!("Failed parsing device: {error:?}");
                                return;
                            }
                        };

                        // Transfer from url_credentials to credentials. Note that `url_credentials` should never overwrride `credentials`,
                        // otherwise, the credentials set during runtime (via rest API) would not overwrride credentials passed via ENV/CLI.
                        {
                            let mut context = context.write().await;
                            if let Some(credential) = context.url_credentials.remove(&device.ip) {
                                trace!("Transferring credential: {credential:?}");
                                let _ =
                                    context.credentials.entry(device.uuid).or_insert(credential);
                            }
                        }

                        Self::handle_device(context, device).await;
                    }
                })
                .await;

            // Remove old cameras
            let mut mcontext = mcontext.write().await;
            mcontext.cameras.retain(|stream_uri, camera| {
                if camera.last_update.elapsed() > 3 * scan_duration {
                    debug!("Stream {stream_uri} removed after not being seen for too long");

                    return false;
                }

                true
            });
        }
    }

    #[instrument(level = "debug", skip(mcontext))]
    async fn handle_device(mcontext: Arc<RwLock<ManagerContext>>, device: OnvifDevice) {
        let _ = mcontext
            .write()
            .await
            .discovered_devices
            .insert(device.uuid, device.clone());

        let credentials = mcontext.read().await.credentials.get(&device.uuid).cloned();

        trace!("Device found, using credentials: {credentials:?}");

        for url in &device.urls {
            let camera = match OnvifCamera::try_new(
                &device,
                &Auth {
                    credentials: credentials.clone(),
                    url: url.clone(),
                },
            )
            .await
            {
                Ok(camera) => camera,
                Err(error) => {
                    let cause = error.root_cause();
                    error!("Failed creating OnvifCamera: {error}: {cause}");
                    continue;
                }
            };

            let Some(streams_informations) =
                &camera.context.read().await.streams_information.clone()
            else {
                error!("Failed getting stream information");
                continue;
            };

            trace!("Stream found: {streams_informations:?}");

            let mut context = mcontext.write().await;
            for stream_information in streams_informations {
                context
                    .cameras
                    .entry(stream_information.stream_uri.to_string())
                    .and_modify(|old_camera| *old_camera = camera.clone())
                    .or_insert_with(|| {
                        trace!("New stream inserted: {stream_information:?}");

                        camera.clone()
                    });
            }
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

        let Some(streams_information) = &camera.context.read().await.streams_information.clone()
        else {
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

        let mut streams_available = vec![];
        for (stream_uri, camera) in mcontext.cameras.iter() {
            let device_information = camera.context.read().await.device_information.clone();

            let name = format!(
                "{model} - {manufacturer} ({hardware_id})",
                model = device_information.model,
                manufacturer = device_information.manufacturer,
                hardware_id = device_information.hardware_id
            );

            let source = VideoSourceOnvifType::Onvif(stream_uri.clone());

            let stream = VideoSourceType::Onvif(VideoSourceOnvif {
                name,
                source,
                device_information,
            });

            streams_available.push(stream);
        }

        streams_available
    }

    #[instrument(level = "debug")]
    pub(crate) async fn remove_camera(device_uuid: uuid::Uuid) -> Result<()> {
        let mcontext = MANAGER.read().await.mcontext.clone();

        let mut cameras_to_remove = vec![];
        {
            let mcontext = mcontext.read().await;

            for (stream_uri, camera) in mcontext.cameras.iter() {
                if camera.context.read().await.device.uuid == device_uuid {
                    cameras_to_remove.push(stream_uri.clone())
                }
            }
        }

        {
            let mut mcontext = mcontext.write().await;

            for stream_uri in cameras_to_remove {
                let _ = mcontext.cameras.remove(&stream_uri);
            }
        }

        Ok(())
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
