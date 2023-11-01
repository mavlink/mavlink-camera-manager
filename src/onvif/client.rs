use onvif::soap;
use onvif_schema::transport;

use anyhow::{anyhow, Context, Result};
use tracing::debug;

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
                        return Err(anyhow!(
                            "advertised device mgmt uri {service_url:?} not expected {devicemgmt_uri:?}"
                        ));
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

    pub async fn get_capabilities(&self) -> Result<()> {
        match onvif_schema::devicemgmt::get_capabilities(&self.devicemgmt, &Default::default())
            .await
        {
            Ok(capabilities) => println!("{:#?}", capabilities),
            Err(error) => println!("Failed to fetch capabilities: {}", error),
        }

        Ok(())
    }

    pub async fn get_device_information(&self) -> Result<(), transport::Error> {
        println!(
            "{:#?}",
            &onvif_schema::devicemgmt::get_device_information(
                &self.devicemgmt,
                &Default::default()
            )
            .await?
        );
        Ok(())
    }

    pub async fn get_service_capabilities(&self) -> Result<()> {
        match onvif_schema::event::get_service_capabilities(&self.devicemgmt, &Default::default())
            .await
        {
            Ok(capability) => println!("devicemgmt: {:#?}", capability),
            Err(error) => println!("Failed to fetch devicemgmt: {}", error),
        }

        if let Some(ref event) = self.event {
            match onvif_schema::event::get_service_capabilities(event, &Default::default()).await {
                Ok(capability) => println!("event: {:#?}", capability),
                Err(error) => println!("Failed to fetch event: {}", error),
            }
        }
        if let Some(ref deviceio) = self.deviceio {
            match onvif_schema::event::get_service_capabilities(deviceio, &Default::default()).await
            {
                Ok(capability) => println!("deviceio: {:#?}", capability),
                Err(error) => println!("Failed to fetch deviceio: {}", error),
            }
        }
        if let Some(ref media) = self.media {
            match onvif_schema::event::get_service_capabilities(media, &Default::default()).await {
                Ok(capability) => println!("media: {:#?}", capability),
                Err(error) => println!("Failed to fetch media: {}", error),
            }
        }
        if let Some(ref media2) = self.media2 {
            match onvif_schema::event::get_service_capabilities(media2, &Default::default()).await {
                Ok(capability) => println!("media2: {:#?}", capability),
                Err(error) => println!("Failed to fetch media2: {}", error),
            }
        }
        if let Some(ref imaging) = self.imaging {
            match onvif_schema::event::get_service_capabilities(imaging, &Default::default()).await
            {
                Ok(capability) => println!("imaging: {:#?}", capability),
                Err(error) => println!("Failed to fetch imaging: {}", error),
            }
        }
        if let Some(ref ptz) = self.ptz {
            match onvif_schema::event::get_service_capabilities(ptz, &Default::default()).await {
                Ok(capability) => println!("ptz: {:#?}", capability),
                Err(error) => println!("Failed to fetch ptz: {}", error),
            }
        }
        if let Some(ref analytics) = self.analytics {
            match onvif_schema::event::get_service_capabilities(analytics, &Default::default())
                .await
            {
                Ok(capability) => println!("analytics: {:#?}", capability),
                Err(error) => println!("Failed to fetch analytics: {}", error),
            }
        }

        Ok(())
    }

    pub async fn get_system_date_and_time(&self) -> Result<()> {
        let date = onvif_schema::devicemgmt::get_system_date_and_time(
            &self.devicemgmt,
            &Default::default(),
        )
        .await;
        println!("{:#?}", date);

        Ok(())
    }

    pub async fn get_stream_uris(&self) -> Result<(), transport::Error> {
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

        let responses = futures_util::future::try_join_all(
            requests
                .iter()
                .map(|r| onvif_schema::media::get_stream_uri(media_client, r)),
        )
        .await?;
        for (p, resp) in profiles.profiles.iter().zip(responses.iter()) {
            println!("token={} name={}", &p.token.0, &p.name.0);
            println!("    {}", &resp.media_uri.uri);
            if let Some(ref v) = p.video_encoder_configuration {
                println!(
                    "    {:?}, {}x{}",
                    v.encoding, v.resolution.width, v.resolution.height
                );
                if let Some(ref r) = v.rate_control {
                    println!("    {} fps, {} kbps", r.frame_rate_limit, r.bitrate_limit);
                }
            }
            if let Some(ref a) = p.audio_encoder_configuration {
                println!(
                    "    audio: {:?}, {} kbps, {} kHz",
                    a.encoding, a.bitrate, a.sample_rate
                );
            }
        }
        Ok(())
    }

    pub async fn get_snapshot_uris(&self) -> Result<(), transport::Error> {
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
                |p: &onvif_schema::onvif::Profile| onvif_schema::media::GetSnapshotUri {
                    profile_token: onvif_schema::onvif::ReferenceToken(p.token.0.clone()),
                },
            )
            .collect();

        let responses = futures_util::future::try_join_all(
            requests
                .iter()
                .map(|r| onvif_schema::media::get_snapshot_uri(media_client, r)),
        )
        .await?;
        for (p, resp) in profiles.profiles.iter().zip(responses.iter()) {
            println!("token={} name={}", &p.token.0, &p.name.0);
            println!("    snapshot_uri={}", &resp.media_uri.uri);
        }
        Ok(())
    }

    pub async fn get_hostname(&self) -> Result<(), transport::Error> {
        let resp =
            onvif_schema::devicemgmt::get_hostname(&self.devicemgmt, &Default::default()).await?;
        debug!("get_hostname response: {:#?}", &resp);
        println!(
            "{}",
            resp.hostname_information
                .name
                .as_deref()
                .unwrap_or("(unset)")
        );
        Ok(())
    }

    pub async fn set_hostname(&self, hostname: String) -> Result<(), transport::Error> {
        onvif_schema::devicemgmt::set_hostname(
            &self.devicemgmt,
            &onvif_schema::devicemgmt::SetHostname { name: hostname },
        )
        .await?;
        Ok(())
    }

    pub async fn enable_analytics(&self) -> Result<(), transport::Error> {
        let media_client = self
            .media
            .as_ref()
            .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
        let mut config =
            onvif_schema::media::get_metadata_configurations(media_client, &Default::default())
                .await?;
        if config.configurations.len() != 1 {
            println!("Expected exactly one analytics config");
            return Ok(());
        }
        let mut c = config.configurations.pop().unwrap();
        let token_str = c.token.0.clone();
        println!("{:#?}", &c);
        if c.analytics != Some(true) || c.events.is_none() {
            println!(
                "Enabling analytics in metadata configuration {}",
                &token_str
            );
            c.analytics = Some(true);
            c.events = Some(onvif_schema::onvif::EventSubscription {
                filter: None,
                subscription_policy: None,
            });
            onvif_schema::media::set_metadata_configuration(
                media_client,
                &onvif_schema::media::SetMetadataConfiguration {
                    configuration: c,
                    force_persistence: true,
                },
            )
            .await?;
        } else {
            println!(
                "Analytics already enabled in metadata configuration {}",
                &token_str
            );
        }

        let profiles = onvif_schema::media::get_profiles(media_client, &Default::default()).await?;
        let requests: Vec<_> = profiles
            .profiles
            .iter()
            .filter_map(
                |p: &onvif_schema::onvif::Profile| match p.metadata_configuration {
                    Some(_) => None,
                    None => Some(onvif_schema::media::AddMetadataConfiguration {
                        profile_token: onvif_schema::onvif::ReferenceToken(p.token.0.clone()),
                        configuration_token: onvif_schema::onvif::ReferenceToken(token_str.clone()),
                    }),
                },
            )
            .collect();
        if !requests.is_empty() {
            println!(
                "Enabling metadata on {}/{} configs",
                requests.len(),
                profiles.profiles.len()
            );
            futures_util::future::try_join_all(
                requests
                    .iter()
                    .map(|r| onvif_schema::media::add_metadata_configuration(media_client, r)),
            )
            .await?;
        } else {
            println!(
                "Metadata already enabled on {} configs",
                profiles.profiles.len()
            );
        }
        Ok(())
    }

    pub async fn get_analytics(&self) -> Result<(), transport::Error> {
        let media_client = self
            .media
            .as_ref()
            .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
        let config = onvif_schema::media::get_video_analytics_configurations(
            media_client,
            &Default::default(),
        )
        .await?;

        println!("{:#?}", &config);
        let c = match config.configurations.first() {
            Some(c) => c,
            None => return Ok(()),
        };
        if let Some(ref a) = self.analytics {
            let mods = onvif_schema::analytics::get_supported_analytics_modules(
                a,
                &onvif_schema::analytics::GetSupportedAnalyticsModules {
                    configuration_token: onvif_schema::onvif::ReferenceToken(c.token.0.clone()),
                },
            )
            .await?;
            println!("{:#?}", &mods);
        }

        Ok(())
    }

    pub async fn get_status(&self) -> Result<(), transport::Error> {
        if let Some(ref ptz) = self.ptz {
            let media_client = match self.media.as_ref() {
                Some(client) => client,
                None => {
                    return Err(transport::Error::Other(
                        "Client media is not available".into(),
                    ))
                }
            };
            let profile = &onvif_schema::media::get_profiles(media_client, &Default::default())
                .await?
                .profiles[0];
            let profile_token = onvif_schema::onvif::ReferenceToken(profile.token.0.clone());
            let status = &onvif_schema::ptz::get_status(
                ptz,
                &onvif_schema::ptz::GetStatus { profile_token },
            )
            .await?;
            println!("ptz status: {:#?}", status);
        }
        Ok(())
    }
}
