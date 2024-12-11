use std::sync::Arc;

use onvif::soap;
use onvif_schema::transport;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::*;

use crate::{stream::gst::utils::get_encode_from_stream_uri, video::types::Format};

use super::manager::OnvifDevice;

#[derive(Clone)]
pub struct OnvifCamera {
    pub context: Arc<RwLock<OnvifCameraContext>>,
    pub last_update: std::time::Instant,
}

pub struct OnvifCameraContext {
    pub device: OnvifDevice,
    pub device_information: OnvifDeviceInformation,
    pub streams_information: Option<Vec<OnvifStreamInformation>>,
    pub credentials: Option<soap::client::Credentials>,
    devicemgmt: soap::client::Client,
    event: Option<soap::client::Client>,
    deviceio: Option<soap::client::Client>,
    media: Option<soap::client::Client>,
    media2: Option<soap::client::Client>,
    imaging: Option<soap::client::Client>,
    ptz: Option<soap::client::Client>,
    analytics: Option<soap::client::Client>,
}

pub struct Auth {
    pub credentials: Option<soap::client::Credentials>,
    pub url: url::Url,
}

#[derive(Debug, Clone)]
pub struct OnvifStreamInformation {
    pub stream_uri: url::Url,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnvifDeviceInformation {
    pub manufacturer: String,
    pub model: String,
    pub firmware_version: String,
    pub serial_number: String,
    pub hardware_id: String,
}

impl OnvifCamera {
    #[instrument(level = "trace", skip(auth))]
    pub async fn try_new(device: &OnvifDevice, auth: &Auth) -> Result<Self> {
        let creds = &auth.credentials;
        let devicemgmt_uri = &auth.url;
        let base_uri = &devicemgmt_uri.origin().ascii_serialization();

        let devicemgmt = soap::client::ClientBuilder::new(devicemgmt_uri)
            .credentials(creds.clone())
            .build();

        let device_information =
            onvif_schema::devicemgmt::get_device_information(&devicemgmt, &Default::default())
                .await
                .map(|i| OnvifDeviceInformation {
                    manufacturer: i.manufacturer,
                    model: i.model,
                    firmware_version: i.firmware_version,
                    serial_number: i.serial_number,
                    hardware_id: i.hardware_id,
                })?;

        let mut context = OnvifCameraContext {
            device: device.clone(),
            device_information,
            streams_information: None,
            credentials: creds.clone(),
            devicemgmt,
            imaging: None,
            ptz: None,
            event: None,
            deviceio: None,
            media: None,
            media2: None,
            analytics: None,
        };

        let services =
            onvif_schema::devicemgmt::get_services(&context.devicemgmt, &Default::default())
                .await
                .context("Failed to get services")?;

        for service in &services.service {
            let service_url = url::Url::parse(&service.x_addr).map_err(anyhow::Error::msg)?;
            if !service_url.as_str().starts_with(base_uri) {
                return Err(anyhow!(
                    "Service URI {service_url:?} is not within base URI {base_uri:?}"
                ));
            }
            let svc = Some(
                soap::client::ClientBuilder::new(&service_url)
                    .credentials(creds.clone())
                    .build(),
            );
            match service.namespace.as_str() {
                "http://www.onvif.org/ver10/device/wsdl" => {
                    if &service_url != devicemgmt_uri {
                        warn!(
                            "advertised device mgmt uri {service_url} not expected {devicemgmt_uri}"
                        );
                    }
                }
                "http://www.onvif.org/ver10/events/wsdl" => context.event = svc,
                "http://www.onvif.org/ver10/deviceIO/wsdl" => context.deviceio = svc,
                "http://www.onvif.org/ver10/media/wsdl" => context.media = svc,
                "http://www.onvif.org/ver20/media/wsdl" => context.media2 = svc,
                "http://www.onvif.org/ver20/imaging/wsdl" => context.imaging = svc,
                "http://www.onvif.org/ver20/ptz/wsdl" => context.ptz = svc,
                "http://www.onvif.org/ver20/analytics/wsdl" => context.analytics = svc,
                _ => trace!("Unknwon service: {service:?}"),
            }
        }

        let context = Arc::new(RwLock::new(context));

        if let Err(error) = Self::update_streams_information(&context).await {
            warn!("Failed to update streams information: {error:?}");
        }

        Ok(Self {
            context,
            last_update: std::time::Instant::now(),
        })
    }

    #[instrument(level = "trace", skip_all)]
    async fn update_streams_information(context: &Arc<RwLock<OnvifCameraContext>>) -> Result<()> {
        let mut context = context.write().await;

        let media_client = context
            .media
            .clone()
            .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;

        // Sometimes a camera responds empty, so we try a couple of times to improve our reliability
        let mut tries = 10;
        let new_streams_information = loop {
            let new_streams_information =
                OnvifCamera::get_streams_information(&media_client, &context.credentials)
                    .await
                    .context("Failed to get streams information")?;

            if new_streams_information.is_empty() {
                if tries == 0 {
                    return Err(anyhow!("No streams information found"));
                }

                tries -= 1;
                continue;
            }

            break new_streams_information;
        };

        context.streams_information.replace(new_streams_information);

        Ok(())
    }

    #[instrument(level = "trace", skip_all)]
    async fn get_streams_information(
        media_client: &soap::client::Client,
        credentials: &Option<soap::client::Credentials>,
    ) -> Result<Vec<OnvifStreamInformation>, transport::Error> {
        let mut streams_information = vec![];

        let profiles = onvif_schema::media::get_profiles(media_client, &Default::default())
            .await?
            .profiles;
        trace!("get_profiles response: {profiles:#?}");

        let requests: Vec<_> = profiles
            .iter()
            .map(
                |p: &onvif_schema::onvif::Profile| onvif_schema::media::GetStreamUri {
                    profile_token: onvif_schema::onvif::ReferenceToken(p.token.0.clone()),
                    stream_setup: onvif_schema::onvif::StreamSetup {
                        stream: onvif_schema::onvif::StreamType::RtpUnicast,
                        transport: onvif_schema::onvif::Transport {
                            protocol: onvif_schema::onvif::TransportProtocol::Rtsp,
                            tunnel: vec![],
                        },
                    },
                },
            )
            .collect();

        let responses = futures::future::try_join_all(
            requests
                .iter()
                .map(|r| onvif_schema::media::get_stream_uri(media_client, r)),
        )
        .await?;
        for (profile, stream_uri_response) in profiles.iter().zip(responses.iter()) {
            trace!("token={} name={}", &profile.token.0, &profile.name.0);
            trace!("\t{}", &stream_uri_response.media_uri.uri);

            let mut stream_uri = match url::Url::parse(&stream_uri_response.media_uri.uri) {
                Ok(stream_url) => stream_url,
                Err(error) => {
                    error!(
                        "Failed to parse stream url: {}, reason: {error:?}",
                        &stream_uri_response.media_uri.uri
                    );
                    continue;
                }
            };

            let Some(video_encoder_configuration) = &profile.video_encoder_configuration else {
                warn!("Skipping uri with no encoders");
                continue;
            };

            if let Some(credentials) = &credentials {
                if stream_uri.set_username(&credentials.username).is_err() {
                    warn!("Failed setting username");
                    continue;
                }

                if stream_uri
                    .set_password(Some(&credentials.password))
                    .is_err()
                {
                    warn!("Failed setting password");
                    continue;
                }

                trace!("Using credentials {credentials:?}");
            }

            let Some(encode) = get_encode_from_stream_uri(&stream_uri).await else {
                warn!("Failed getting encoding from RTSP stream at {stream_uri}");
                continue;
            };

            let video_rate = video_encoder_configuration
                .rate_control
                .as_ref()
                .map(|rate_control| {
                    (rate_control.frame_rate_limit as f32
                        / rate_control.encoding_interval.max(1) as f32) as u32
                })
                .unwrap_or_default();

            let intervals = vec![crate::video::types::FrameInterval {
                numerator: 1,
                denominator: video_rate,
            }];

            let sizes = vec![crate::video::types::Size {
                width: video_encoder_configuration.resolution.width.max(0) as u32,
                height: video_encoder_configuration.resolution.height.max(0) as u32,
                intervals,
            }];

            let format = Format { encode, sizes };

            streams_information.push(OnvifStreamInformation { stream_uri, format });
        }

        Ok(streams_information)
    }
}
