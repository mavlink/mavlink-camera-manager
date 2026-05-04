use tokio::sync::mpsc;
use tracing::*;

use crate::zenoh::foxglove_messages::Log;

pub struct ZenohLogPublisher {
    _publisher_task_handle: tokio::task::JoinHandle<()>,
    sender: mpsc::Sender<Log>,
}

impl Drop for ZenohLogPublisher {
    fn drop(&mut self) {
        self._publisher_task_handle.abort();
    }
}

const ZENOH_INIT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

impl ZenohLogPublisher {
    pub fn new(topic: &str, use_cdr: bool) -> Self {
        let (sender, mut receiver) = mpsc::channel(10_000);

        let handle = tokio::spawn({
            let topic = topic.to_owned();

            async move {
                let session = loop {
                    let Some(session) = crate::zenoh::get() else {
                        warn!(
                            "Failed getting zenoh session, retrying Zenoh publisher initialization in {ZENOH_INIT_RETRY_DELAY:?}...",
                        );
                        tokio::time::sleep(ZENOH_INIT_RETRY_DELAY).await;
                        continue;
                    };
                    break session;
                };

                let publisher = loop {
                    match session.declare_keyexpr(&topic).await {
                        Ok(zenoh_topic) => {
                            let encoding = if use_cdr {
                                zenoh::bytes::Encoding::APPLICATION_CDR.with_schema("foxglove.Log")
                            } else {
                                zenoh::bytes::Encoding::APPLICATION_JSON.with_schema("foxglove.Log")
                            };

                            match session
                                .declare_publisher(zenoh_topic.clone())
                                .encoding(encoding)
                                .await
                            {
                                Ok(publisher) => break publisher,
                                Err(error) => {
                                    error!(
                                    "Failed to declare Zenoh publisher for '{topic:?}': {error:?}",
                                );
                                }
                            }
                        }
                        Err(error) => {
                            error!("Failed to declare Zenoh keyexpr '{topic:?}': {error:?}");
                        }
                    }
                    warn!(
                        "Retrying Zenoh publisher initialization in {ZENOH_INIT_RETRY_DELAY:?}...",
                    );
                    tokio::time::sleep(ZENOH_INIT_RETRY_DELAY).await;
                };

                info!("Zenoh log publisher successfully started for topic: {topic:?}",);

                while let Some(log) = receiver.recv().await {
                    let payload: Vec<u8> = if use_cdr {
                        match cdr::serialize::<_, _, cdr::CdrLe>(&log, cdr::Infinite) {
                            Ok(data) => data,
                            Err(error) => {
                                error!("CDR serialization failed: {error:?}");
                                continue;
                            }
                        }
                    } else {
                        match serde_json::to_vec(&log) {
                            Ok(data) => data,
                            Err(error) => {
                                error!("JSON serialization failed: {error:?}");
                                continue;
                            }
                        }
                    };

                    if let Err(error) = publisher.put(payload).await {
                        error!("Failed to publish log to Zenoh topic '{topic:?}': {error:?}")
                    }
                }

                if let Err(error) = publisher.undeclare().await {
                    warn!("Failed to undeclare Zenoh publisher for '{topic:?}': {error:?}");
                }
            }
        });

        Self {
            _publisher_task_handle: handle,
            sender,
        }
    }

    pub fn sender(&self) -> mpsc::Sender<Log> {
        self.sender.clone()
    }
}
