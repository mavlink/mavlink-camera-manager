use onvif::soap;
use onvif_schema::{devicemgmt::GetDeviceInformationResponse, transport};

use anyhow::{anyhow, Result};
use tracing::*;

pub struct Clients {
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
    pub url: Box<str>,
}

impl Clients {
    #[instrument(level = "debug", skip(auth))]
    pub async fn try_new(auth: &Auth) -> Result<Self> {
        let creds = &auth.credentials;
        let devicemgmt_uri = url::Url::parse(&auth.url)?;
        let base_uri = &devicemgmt_uri.origin().ascii_serialization();

        let mut this = Self {
            devicemgmt: soap::client::ClientBuilder::new(&devicemgmt_uri)
                .credentials(creds.clone())
                .build(),
            imaging: None,
            ptz: None,
            event: None,
            deviceio: None,
            media: None,
            media2: None,
            analytics: None,
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
                    if service_url != devicemgmt_uri {
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
                _ => debug!("unknown service: {:?}", service),
            }
        }

        Ok(this)
    }

    #[instrument(level = "debug", skip(self))]
    pub async fn get_device_information(
        &self,
    ) -> Result<GetDeviceInformationResponse, transport::Error> {
        onvif_schema::devicemgmt::get_device_information(&self.devicemgmt, &Default::default())
            .await
    }

    pub async fn get_stream_uris(&self) -> Result<Vec<url::Url>, transport::Error> {
        let mut urls: Vec<url::Url> = vec![];
        let media_client = self
            .media
            .as_ref()
            .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
        let profiles = onvif_schema::media::get_profiles(media_client, &Default::default()).await?;
        debug!("get_profiles response: {:#?}", &profiles);
        let requests: Vec<_> = profiles
            .profiles
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
        for (p, resp) in profiles.profiles.iter().zip(responses.iter()) {
            debug!("token={} name={}", &p.token.0, &p.name.0);
            debug!("    {}", &resp.media_uri.uri);
            match url::Url::parse(&resp.media_uri.uri) {
                Ok(address) => urls.push(address),
                Err(error) => {
                    error!(
                        "Failed to parse stream url: {}, reason: {error:?}",
                        &resp.media_uri.uri
                    )
                }
            }
            if let Some(ref v) = p.video_encoder_configuration {
                debug!(
                    "    {:?}, {}x{}",
                    v.encoding, v.resolution.width, v.resolution.height
                );
                if let Some(ref r) = v.rate_control {
                    debug!("    {} fps, {} kbps", r.frame_rate_limit, r.bitrate_limit);
                }
            }
            if let Some(ref a) = p.audio_encoder_configuration {
                debug!(
                    "    audio: {:?}, {} kbps, {} kHz",
                    a.encoding, a.bitrate, a.sample_rate
                );
            }
        }
        Ok(urls)
    }
}
