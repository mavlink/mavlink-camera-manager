use onvif::soap;
use onvif_schema::{devicemgmt::GetDeviceInformationResponse, transport};

use anyhow::{anyhow, Result};
use tracing::*;

use crate::{stream::gst::utils::get_encode_from_rtspsrc, video::types::Format};

#[derive(Clone)]
pub struct OnvifCamera {
    devicemgmt: soap::client::Client,
    event: Option<soap::client::Client>,
    deviceio: Option<soap::client::Client>,
    media: Option<soap::client::Client>,
    media2: Option<soap::client::Client>,
    imaging: Option<soap::client::Client>,
    ptz: Option<soap::client::Client>,
    analytics: Option<soap::client::Client>,
    pub streams_information: Option<Vec<OnvifStreamInformation>>,
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

impl OnvifCamera {
    #[instrument(level = "trace", skip(auth))]
    pub async fn try_new(auth: &Auth) -> Result<Self> {
        let creds = &auth.credentials;
        let devicemgmt_uri = &auth.url;
        let base_uri = &devicemgmt_uri.origin().ascii_serialization();

        let mut this = Self {
            devicemgmt: soap::client::ClientBuilder::new(devicemgmt_uri)
                .credentials(creds.clone())
                .build(),
            imaging: None,
            ptz: None,
            event: None,
            deviceio: None,
            media: None,
            media2: None,
            analytics: None,
            streams_information: None,
        };

        let services =
            onvif_schema::devicemgmt::get_services(&this.devicemgmt, &Default::default()).await?;

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
                "http://www.onvif.org/ver10/events/wsdl" => this.event = svc,
                "http://www.onvif.org/ver10/deviceIO/wsdl" => this.deviceio = svc,
                "http://www.onvif.org/ver10/media/wsdl" => this.media = svc,
                "http://www.onvif.org/ver20/media/wsdl" => this.media2 = svc,
                "http://www.onvif.org/ver20/imaging/wsdl" => this.imaging = svc,
                "http://www.onvif.org/ver20/ptz/wsdl" => this.ptz = svc,
                "http://www.onvif.org/ver20/analytics/wsdl" => this.analytics = svc,
                _ => trace!("unknown service: {:?}", service),
            }
        }

        this.streams_information
            .replace(this.get_streams_information().await?);

        Ok(this)
    }

    #[instrument(level = "trace", skip(self))]
    pub async fn get_device_information(
        &self,
    ) -> Result<GetDeviceInformationResponse, transport::Error> {
        onvif_schema::devicemgmt::get_device_information(&self.devicemgmt, &Default::default())
            .await
    }

    async fn get_streams_information(
        &self,
    ) -> Result<Vec<OnvifStreamInformation>, transport::Error> {
        let mut streams_information = vec![];

        let media_client = self
            .media
            .as_ref()
            .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
        let profiles = onvif_schema::media::get_profiles(media_client, &Default::default())
            .await?
            .profiles;
        trace!("get_profiles response: {:#?}", &profiles);
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

            let stream_uri = match url::Url::parse(&stream_uri_response.media_uri.uri) {
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

            let Some(encode) = get_encode_from_rtspsrc(&stream_uri).await else {
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
