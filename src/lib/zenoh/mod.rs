use anyhow::{Result, anyhow};
use tokio::sync::OnceCell;
use tracing::*;
use zenoh::{Config, Session, config::ZenohId};

static SESSION: OnceCell<Session> = OnceCell::const_new();

#[instrument(level = "debug")]
pub async fn init() -> Result<()> {
    SESSION
        .get_or_try_init(|| async {
            debug!("Starting zenoh service.");

            let config = load_config()?;

            trace!("Using Zenoh config: {config:#?}");

            let session = zenoh::open(config)
                .await
                .map_err(|error| anyhow!("Failed to open Zenoh session: {error:?}"))?;

            let info = session.info();
            let zid = info.zid().await;
            let routers = info.routers_zid().await.collect::<Vec<ZenohId>>();

            info!("Zenoh Session started with zid: {zid:?}, routers: {routers:?}",);

            anyhow::Ok(session)
        })
        .await?;

    Ok(())
}

#[instrument(level = "debug")]
pub fn get() -> Option<Session> {
    SESSION.get().cloned()
}

#[instrument(level = "debug")]
fn load_config() -> Result<Config> {
    let mut config = if let Some(zenoh_config_file) = crate::cli::manager::zenoh_config_file() {
        Config::from_file(zenoh_config_file)
            .map_err(|error| anyhow!("Failed to load Zenoh config file: {error:?}"))?
    } else {
        let mut config = Config::default();
        config
            .insert_json5("mode", r#""client""#)
            .expect("Failed to insert client mode");
        config
            .insert_json5("connect/endpoints", r#"["tcp/127.0.0.1:7447"]"#)
            .expect("Failed to insert endpoints");
        config
    };

    let name = env!("CARGO_PKG_NAME");

    config
        .insert_json5("transport/link/tx/queue/size/real_time", "16")
        .expect("Failed to insert tx queue size");
    config
        .insert_json5("adminspace", r#"{"enabled": true}"#)
        .expect("Failed to insert adminspace");
    config
        .insert_json5("metadata", &format!(r#"{{"name": "{name}"}}"#))
        .expect("Failed to insert metadata");

    Ok(config)
}
