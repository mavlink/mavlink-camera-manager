use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt as _;
use onvif::soap::client::Credentials;
use tokio::sync::RwLock;
use tracing::*;

use mcm_api::v1::{
    server::OnvifDevice,
    video::{Format, VideoSourceOnvif, VideoSourceOnvifType, VideoSourceType},
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
        let scan_duration = Duration::from_secs(20);
        let retry_delay = Duration::from_secs(1);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            trace!("Starting ONVIF discovery cycle across all suitable interfaces...");

            // 1. Find suitable interfaces
            let suitable_interfaces = Self::find_suitable_interfaces();
            trace!(
                "Found {} suitable interface(s) for ONVIF discovery.",
                suitable_interfaces.len()
            );

            if suitable_interfaces.is_empty() {
                trace!("No suitable interfaces found. Sleeping before next cycle...");
                tokio::time::sleep(retry_delay).await;
                continue; // Skip to the next iteration of the loop
            }

            // 2. Spawn discovery tasks for each interface
            let mut discovery_tasks = Vec::new();
            for (iface_name, ip_addr) in suitable_interfaces {
                trace!(
                    "Spawning ONVIF discovery task for interface '{iface_name}' (IP: {ip_addr})..."
                );

                let task_handle = tokio::spawn({
                    let mcontext = mcontext.clone();

                    async move {
                        match Self::run_discovery_on_interface(ip_addr, scan_duration).await {
                            Ok(devices_stream) => {
                                trace!("Task result: ONVIF discovery on {ip_addr} successful.");
                                // Process devices found on this interface within the task
                                Self::process_discovered_devices(mcontext, devices_stream, ip_addr)
                                    .await;
                                trace!(
                                    "Task finished: Processing devices from {ip_addr} completed."
                                );
                                Ok::<(), onvif::discovery::Error>(()) // Indicate success (discovery part)
                            }
                            Err(error) => {
                                warn!("Task failed: ONVIF discovery failed on interface IP '{ip_addr}': {error:?}");
                                Err(error)
                            }
                        }
                    }
                });
                discovery_tasks.push((iface_name, ip_addr, task_handle));
            }

            // 3. Wait for all discovery tasks to complete
            trace!(
                "Waiting for {} discovery tasks to complete...",
                discovery_tasks.len()
            );
            for (iface_name, ip_addr, task_handle) in discovery_tasks {
                match task_handle.await {
                    Ok(Ok(())) => {
                        trace!("Discovery task for interface '{iface_name}' (IP: {ip_addr}) completed successfully.");
                    }
                    Ok(Err(discovery_error)) => {
                        // The task ran, but the discovery itself failed
                        warn!("Discovery task for interface '{iface_name}' (IP: {ip_addr}) reported failure: {discovery_error:?}");
                    }
                    Err(join_error) => {
                        // The task itself panicked or was cancelled
                        error!("Discovery task for interface '{iface_name}' (IP: {ip_addr}) panicked or was cancelled: {join_error:?}");
                    }
                }
            }
            trace!("All discovery tasks have completed.");

            // 4. Cleanup old cameras (once, after all discoveries)
            Self::cleanup_old_cameras(mcontext.clone(), scan_duration).await;

            trace!("Completed ONVIF discovery cycle. Sleeping before next cycle...");
            tokio::time::sleep(retry_delay).await;
        }
    }

    fn find_suitable_interfaces() -> Vec<(String, IpAddr)> {
        let interfaces = pnet::datalink::interfaces();

        trace!(
            "Evaluating {} network interface(s) for suitability...",
            interfaces.len()
        );

        let suitable_set: HashSet<(String, IpAddr)> = interfaces
            .iter()
            .filter_map(|interface| {
                // Check basic interface state and capabilities
                if !(interface.is_up() && !interface.is_loopback() && interface.is_multicast()) {
                    trace!(
                        "Skipping interface '{}': Up={}, Loopback={}, Multicast={}",
                        interface.name,
                        interface.is_up(),
                        interface.is_loopback(),
                        interface.is_multicast()
                    );
                    return None;
                }

                trace!(
                    "Interface '{}' is suitable for ONVIF discovery, checking its addresses...",
                    interface.name
                );

                // Create an iterator of (Name, IpAddr) tuples for each IPv4 address on this interface
                let ipv4_tuples = interface.ips.iter().filter_map(|ip_network| {
                    if let pnet::ipnetwork::IpNetwork::V4(ipv4_network) = ip_network {
                        let ip_addr = IpAddr::V4(ipv4_network.ip());
                        trace!(
                            "Found suitable IPv4 address on interface '{}': {} (Subnet: {})",
                            interface.name,
                            ip_addr,
                            ipv4_network
                        );
                        // Return Some tuple, which will be collected and deduplicated by the HashSet
                        Some((interface.name.clone(), ip_addr))
                    } else {
                        None // Ignore non-IPv4 addresses
                    }
                });

                Some(ipv4_tuples)
            })
            .flatten()
            .collect();

        if suitable_set.is_empty() {
            warn!("No suitable network interfaces found for ONVIF discovery.");
        } else {
            trace!(
                "Found {} unique suitable interface IP address(es) for ONVIF discovery.",
                suitable_set.len()
            );
        }

        suitable_set.into_iter().collect()
    }

    #[instrument(level = "debug", skip(scan_duration))]
    async fn run_discovery_on_interface(
        listen_ip: IpAddr,
        scan_duration: tokio::time::Duration,
    ) -> Result<
        futures::stream::BoxStream<'static, onvif::discovery::Device>,
        onvif::discovery::Error,
    > {
        trace!("Task started: Initiating ONVIF discovery on interface IP '{listen_ip}'...");

        let devices_stream = onvif::discovery::DiscoveryBuilder::default()
            .listen_address(listen_ip)
            .duration(scan_duration)
            .run()
            .await?;

        Ok(Box::pin(devices_stream))
    }

    #[instrument(level = "debug", skip(mcontext, devices_stream))]
    async fn process_discovered_devices(
        mcontext: Arc<RwLock<ManagerContext>>,
        devices_stream: futures::stream::BoxStream<'static, onvif::discovery::Device>,
        ip_addr: IpAddr,
    ) {
        const MAX_CONCURRENT_JUMPERS: usize = 100;
        trace!(
            "Collecting and processing discovered devices from stream for address {ip_addr:?}..."
        );

        let devices_vec: Vec<_> = devices_stream.collect().await;
        trace!(
            "Found {} raw device candidate(s) in this discovery cycle.",
            devices_vec.len()
        );

        futures::stream::iter(devices_vec)
            .for_each_concurrent(MAX_CONCURRENT_JUMPERS, |device| {
                let context = mcontext.clone();
                async move {
                    trace!(
                        "Processing discovered device candidate: {:?}",
                        device.address
                    );

                    let device_address = device.address.clone(); // Save address for potential error logging
                    let device = match onvif_device_from_discovery(device) {
                        Ok(onvif_device) => onvif_device,
                        Err(error) => {
                            error!(
                                "Failed parsing device candidate (address: {device_address:?}): {error:?}"
                            );
                            return;
                        }
                    };

                    let device_uuid = device.uuid;
                    let device_ip = device.ip;
                    trace!(
                        "Successfully parsed device candidate into OnvifDevice: UUID={device_uuid:?}, IP={device_ip}"
                    );

                    {
                        let mut context = context.write().await;
                        if let Some(credential) = context.url_credentials.remove(&device_ip) {
                            trace!("Transferring credential for IP {device_ip}: {credential:?}");
                            let _ = context.credentials.entry(device_uuid).or_insert(credential);
                        }
                    }

                    trace!(
                        "Calling handle_device for parsed OnvifDevice UUID={device_uuid:?}, IP={device_ip}"
                    );
                    Self::handle_device(context, device).await;

                    trace!(
                        "Finished processing OnvifDevice UUID={device_uuid:?}, IP={device_ip}"
                    );
                }
            })
            .await;
        trace!("Finished processing devices for this interface.");
    }

    #[instrument(level = "debug", skip(mcontext))]
    async fn cleanup_old_cameras(
        mcontext: Arc<RwLock<ManagerContext>>,
        scan_duration: tokio::time::Duration,
    ) {
        trace!("Cleaning up old camera entries...");
        let mut context = mcontext.write().await;
        let initial_camera_count = context.cameras.len();
        context.cameras.retain(|stream_uri, camera| {
            let elapsed = camera.last_update.elapsed();
            let threshold = 3 * scan_duration;
            if elapsed > threshold {
                trace!(
                    "Removing stale stream {stream_uri} (last seen {elapsed:?} ago, threshold {threshold:?})"
                );
                false
            } else {
                true
            }
        });
        let removed_count = initial_camera_count - context.cameras.len();
        if removed_count > 0 {
            debug!(
                "Removed {removed_count} stale camera stream(s). Cameras map now has {} entries.",
                context.cameras.len()
            );
        } else {
            trace!(
                "No stale camera streams removed. Cameras map has {} entries.",
                context.cameras.len()
            );
        }
    }

    #[instrument(level = "debug", skip(mcontext))]
    async fn handle_device(mcontext: Arc<RwLock<ManagerContext>>, device: OnvifDevice) {
        let device_uuid = device.uuid;
        let device_ip = device.ip;
        trace!("handle_device called for UUID={device_uuid:?}, IP={device_ip}");

        let existing_entry = mcontext
            .write()
            .await
            .discovered_devices
            .insert(device_uuid, device.clone());

        if existing_entry.is_some() {
            trace!("Updated existing entry for discovered device UUID={device_uuid:?}");
        } else {
            trace!("Added new entry for discovered device UUID={device_uuid:?}");
        }

        let credentials = mcontext.read().await.credentials.get(&device_uuid).cloned();
        trace!("Device credentials lookup result for UUID={device_uuid:?}: {credentials:?}");

        // Iterate through the device's URLs (e.g., different service endpoints)
        trace!("Attempting to create OnvifCamera instances for device UUID={device_uuid:?}, IP={device_ip}, found {} URL(s)", device.urls.len());
        for (index, url) in device.urls.iter().enumerate() {
            trace!(
                "Processing URL {}/{} for device UUID={device_uuid:?}: {url}",
                index + 1,
                device.urls.len(),
            );

            let camera = match OnvifCamera::try_new(
                &device,
                &Auth {
                    credentials: credentials.clone(),
                    url: url.clone(),
                },
            )
            .await
            {
                Ok(camera) => {
                    trace!("Successfully created OnvifCamera instance from URL: {url}");
                    camera
                }
                Err(error) => {
                    let cause = error.root_cause();
                    warn!(
                        "Failed creating OnvifCamera from URL {url}: {error} (Root cause: {cause})"
                    );
                    continue; // Try the next URL for this device
                }
            };

            // Get stream information from the successfully created camera instance
            let streams_information_option =
                camera.context.read().await.streams_information.clone();
            trace!(
                "Retrieved streams_information for camera from URL {url}: {streams_information_option:?}"
            );

            let Some(streams_informations) = streams_information_option else {
                error!("Failed getting stream information (streams_information is None) for camera created from URL: {url}");
                continue; // Move to the next URL
            };

            trace!(
                "Found {} stream(s) for camera from URL {url}",
                streams_informations.len()
            );

            // Update the manager's camera map with information from this camera instance
            let mut context = mcontext.write().await;
            for (stream_index, stream_information) in streams_informations.iter().enumerate() {
                trace!(
                    "Processing stream {}/{} from URL {url}: {:?}",
                    stream_index + 1,
                    streams_informations.len(),
                    stream_information.stream_uri
                );

                // Create an unauthenticated version of the stream URI for the map key
                let mut unauthenticated_stream_uri = stream_information.stream_uri.clone();
                unauthenticated_stream_uri.set_password(None).unwrap();
                unauthenticated_stream_uri.set_username("").unwrap();
                let unauth_uri_string = unauthenticated_stream_uri.to_string();

                // Insert or update the camera in the map
                let is_new_entry = !context.cameras.contains_key(&unauth_uri_string);
                context
                    .cameras
                    .entry(unauth_uri_string.clone())
                    .and_modify(|old_camera| {
                        trace!(
                            "Updating existing camera entry for stream URI: {unauth_uri_string}"
                        );
                        *old_camera = camera.clone()
                    })
                    .or_insert_with(|| {
                        trace!("Inserting new camera entry for stream URI: {unauth_uri_string}");
                        camera.clone()
                    });

                if is_new_entry {
                    info!("New ONVIF camera stream discovered and added: {unauth_uri_string}");
                } else {
                    trace!("ONVIF camera stream updated: {unauth_uri_string}");
                }
            }
        }
        trace!("handle_device finished for UUID={device_uuid:?}, IP={device_ip}");
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
            .find(|stream_information| {
                let mut unauthenticated_stream_uri = stream_information.stream_uri.clone();
                let _ = unauthenticated_stream_uri.set_password(None);
                let _ = unauthenticated_stream_uri.set_username("");
                unauthenticated_stream_uri.to_string() == *stream_uri
            })
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

    #[instrument(level = "debug")]
    pub async fn reset() -> Result<()> {
        let mut manager = MANAGER.write().await;
        manager._task.abort();

        *manager = Manager::default();

        Ok(())
    }
}

/// Address must be something like `urn:uuid:bc071801-c50f-8301-ac36-bc071801c50f`.
/// Read 7 Device discovery from [ONVIF-Core-Specification](https://www.onvif.org/specs/core/ONVIF-Core-Specification-v1612a.pdf)
#[instrument(level = "debug")]
fn device_address_to_uuid(device_address: &str) -> Result<uuid::Uuid> {
    device_address
        .split(':')
        .next_back()
        .context("Failed to parse device address into a UUID")?
        .parse()
        .map_err(anyhow::Error::msg)
}

pub fn onvif_device_from_discovery(device: onvif::discovery::Device) -> Result<OnvifDevice> {
    Ok(OnvifDevice {
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
