use std::{
    collections::HashMap,
    sync::{Arc, RwLock, TryLockError},
};

use crate::{
    settings,
    stream::{types::CaptureConfiguration, webrtc::signalling_protocol::BindAnswer},
    video::video_source,
};
use crate::{stream::sink::SinkInterface, video::types::VideoSourceType};
use crate::{
    stream::sink::{webrtc_sink::WebRTCSink, Sink},
    video_stream::types::VideoAndStreamInformation,
};

use anyhow::{anyhow, Context, Error, Result};

type ClonableResult<T> = Result<T, Arc<Error>>;

use cached::proc_macro::cached;
use tracing::*;

use super::{
    pipeline::PipelineGstreamerInterface,
    types::StreamStatus,
    webrtc::{
        self,
        signalling_protocol::RTCSessionDescription,
        signalling_server::{StreamManagementInterface, WebRTCSessionManagementInterface},
    },
    Stream,
};

#[derive(Default)]
pub struct Manager {
    streams: HashMap<uuid::Uuid, Stream>,
}

lazy_static! {
    static ref MANAGER: Arc<RwLock<Manager>> = Default::default();
}

impl Manager {
    #[instrument(level = "debug", skip(self))]
    fn update_settings(&self) {
        let video_and_stream_informations = self
            .streams
            .values()
            .filter_map(|stream| match stream.state.read() {
                Ok(guard) => Some(guard.video_and_stream_information.clone()),
                Err(error) => {
                    error!("Failed locking a Mutex. Reason: {error}");
                    None
                }
            })
            .collect::<Vec<VideoAndStreamInformation>>();

        settings::manager::set_streams(video_and_stream_informations.as_slice());
    }
}

#[instrument(level = "debug")]
pub fn init() {
    debug!("Starting video stream service.");

    if let Err(error) = gst::init() {
        error!("Error! {error}");
    };

    config_gst_plugins();
}

#[instrument(level = "debug")]
fn config_gst_plugins() {
    let plugins_config = crate::cli::manager::gst_feature_rank();

    for config in plugins_config {
        match crate::stream::gst::utils::set_plugin_rank(config.name.as_str(), config.rank) {
            Ok(_) => info!(
                "Gstreamer Plugin {name:?} configured with rank {rank:?}.",
                name = config.name,
                rank = config.rank,
            ),
            Err(error) => error!("Error when trying to configure plugin {name:?} rank to {rank:?}. Reason: {error:?}", name = config.name, rank = config.rank, error=error.to_string()),
        }
    }
}

pub fn remove_all_streams() -> Result<()> {
    let keys = {
        let mut ids = vec![];

        match MANAGER.read() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        }
        .streams
        .keys()
        .for_each(|id| {
            ids.push(*id);
        });

        ids
    };

    keys.iter().for_each(|stream_id| {
        if let Err(error) = Manager::remove_stream(stream_id) {
            warn!("Failed removing stream {stream_id:?}: {error:?}")
        }
    });

    Ok(())
}

#[instrument(level = "debug")]
pub async fn start_default() -> Result<()> {
    // Get streams from default settings, this needs to be done first because
    // remove_all_streams will modify the settings as its using the stream manager
    // to remove the streams, and the stream manager will save the state after
    // each removal in the settings
    let mut streams = settings::manager::streams();

    // Gently remove all streams as we are going to replace the entire list below
    remove_all_streams()?;

    // Update all local video sources to make sure that they are available
    let mut candidates = video_source::cameras_available();
    update_devices(&mut streams, &mut candidates, true);

    // Remove all invalid video_sources
    let streams: Vec<VideoAndStreamInformation> = streams
        .into_iter()
        .filter(|stream| stream.video_source.inner().is_valid())
        .collect();

    debug!("Streams: {streams:#?}");

    for stream in streams {
        if let Err(error) = add_stream_and_start(stream).await {
            error!("Not possible to start stream: {error:?}");
        };
    }

    Ok(())
}

#[instrument(level = "debug")]
pub fn update_devices(
    streams: &mut Vec<VideoAndStreamInformation>,
    candidates: &mut Vec<VideoSourceType>,
    verbose: bool,
) {
    for stream in streams {
        let VideoSourceType::Local(source) = &mut stream.video_source else {
            continue;
        };
        let CaptureConfiguration::Video(capture_configuration) =
            &stream.stream_information.configuration
        else {
            continue;
        };

        match source.try_identify_device(capture_configuration, candidates) {
            Ok(Some(candidate_source_string)) => {
                let Some((idx, candidate)) =
                    candidates.iter().enumerate().find_map(|(idx, candidate)| {
                        (candidate.inner().source_string() == candidate_source_string)
                            .then_some((idx, candidate))
                    })
                else {
                    error!("CRITICAL: The device was identified as {candidate_source_string:?}, but it is not the candidates list"); // This shouldn't ever be reachable, otherwise the above logic is flawed
                    continue;
                };

                let VideoSourceType::Local(camera) = candidate else {
                    error!("CRITICAL: The device was identified as {candidate_source_string:?}, but it is not a Local device"); // This shouldn't ever be reachable, otherwise the above logic is flawed
                    continue;
                };
                *source = camera.clone();
                // Only remove the candidate from the list after using it, avoiding the wrong but possible logic from the CRITICAL erros branches above, the additional cost is this clone
                candidates.remove(idx);
            }
            Err(reason) => {
                // Invalidate the device
                source.device_path = "".into();
                if verbose {
                    warn!("Device {source:?} was invalidated. Reason: {reason:?}");
                };
            }
            _ => (),
        }
    }
}

#[instrument(level = "debug")]
pub fn streams() -> Result<Vec<StreamStatus>> {
    Manager::streams_information()
}

#[instrument(level = "debug")]
#[cached(time = 1)]
pub fn get_first_sdp_from_source(source: String) -> ClonableResult<gst_sdp::SDPMessage> {
    let manager = match MANAGER.read() {
        Ok(guard) => guard,
        Err(error) => return Err(Arc::new(anyhow!("Failed locking a Mutex. Reason: {error}"))),
    };

    let Some(result) = manager.streams.values().find_map(|stream| {
        let state = match stream.state.read() {
            Ok(guard) => guard,
            Err(error) => {
                error!("Failed locking a Mutex. Reason: {error}");
                return None;
            }
        };

        if state
            .video_and_stream_information
            .video_source
            .inner()
            .source_string()
            == source
        {
            state
                .pipeline
                .inner_state_as_ref()
                .sinks
                .values()
                .find_map(|sink| sink.get_sdp().ok())
        } else {
            None
        }
    }) else {
        return Err(Arc::new(anyhow!(
            "Failed to find any valid sdp for souce {source:?}"
        )));
    };
    Ok(result)
}

#[instrument(level = "debug")]
#[cached(time = 1)]
pub async fn get_jpeg_thumbnail_from_source(
    source: String,
    quality: u8,
    target_height: Option<u32>,
) -> Option<ClonableResult<Vec<u8>>> {
    // TODO: Fix this: both `manager` and `state` are locked during the `make_jpeg_thumbnail_from_last_frame`'s await
    let manager = loop {
        match MANAGER.try_read() {
            Ok(guard) => break guard,
            Err(TryLockError::WouldBlock) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                continue;
            }
            _ => {
                error!("Failed accessing MANAGER");
                return None;
            }
        }
    };

    let mut streams = manager.streams.values();
    let state = loop {
        if let Some(stream) = streams.next() {
            let state = loop {
                match stream.state.try_read() {
                    Ok(guard) => break guard,
                    Err(TryLockError::WouldBlock) => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        continue;
                    }
                    _ => {
                        error!("Failed accessing stream state");
                        return None;
                    }
                }
            };

            if state
                .video_and_stream_information
                .video_source
                .inner()
                .source_string()
                == source
            {
                break state;
            }
        } else {
            return None;
        }
    };

    let Some(Sink::Image(image_sink)) = state
        .pipeline
        .inner_state_as_ref()
        .sinks
        .values()
        .find(|sink| matches!(sink, Sink::Image(_)))
    else {
        return None;
    };

    Some(
        image_sink
            .make_jpeg_thumbnail_from_last_frame(quality, target_height)
            .await
            .map_err(Arc::new),
    )
}

#[instrument(level = "debug")]
pub async fn add_stream_and_start(
    video_and_stream_information: VideoAndStreamInformation,
) -> Result<()> {
    {
        let manager = match MANAGER.read() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };
        for stream in manager.streams.values() {
            let state = match stream.state.read() {
                Ok(guard) => guard,
                Err(error) => {
                    return Err(anyhow!("Failed locking a Mutex. Reason: {error}"));
                }
            };

            state
                .video_and_stream_information
                .conflicts_with(&video_and_stream_information)?;
        }
    }

    let stream = Stream::try_new(&video_and_stream_information).await?;
    Manager::add_stream(stream)?;

    Ok(())
}

#[instrument(level = "debug")]
pub fn remove_stream_by_name(stream_name: &str) -> Result<()> {
    let manager = match MANAGER.read() {
        Ok(guard) => guard,
        Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
    };
    if let Some(stream_id) = &manager.streams.iter().find_map(|(id, stream)| {
        let state = match stream.state.read() {
            Ok(guard) => guard,
            Err(error) => {
                error!("Failed locking a Mutex. Reason: {error}");
                return None;
            }
        };

        if state.video_and_stream_information.name == *stream_name {
            return Some(*id);
        }
        None
    }) {
        drop(manager);
        Manager::remove_stream(stream_id)?;
        return Ok(());
    }

    Err(anyhow!("Stream named {stream_name:?} not found"))
}

impl WebRTCSessionManagementInterface for Manager {
    #[instrument(level = "debug")]
    fn add_session(
        bind: &webrtc::signalling_protocol::BindOffer,
        sender: tokio::sync::mpsc::UnboundedSender<Result<webrtc::signalling_protocol::Message>>,
    ) -> Result<webrtc::signalling_protocol::SessionId> {
        let mut manager = match MANAGER.write() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let producer_id = bind.producer_id;
        let consumer_id = bind.consumer_id;
        let session_id = Self::generate_uuid();

        let stream = manager.streams.get_mut(&producer_id).context(format!(
            "Cannot find any stream with producer {producer_id:?}"
        ))?;

        let bind = BindAnswer {
            producer_id,
            consumer_id,
            session_id,
        };

        let sink = Sink::WebRTC(WebRTCSink::try_new(bind, sender)?);
        stream.state.write().unwrap().pipeline.add_sink(sink)?;
        debug!("WebRTC session created: {session_id:?}");

        Ok(session_id)
    }

    #[instrument(level = "debug")]
    fn remove_session(
        bind: &webrtc::signalling_protocol::BindAnswer,
        _reason: String,
    ) -> Result<()> {
        let mut manager = match MANAGER.write() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let stream = manager
            .streams
            .get_mut(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?;

        let mut state = match stream.state.write() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        state
            .pipeline
            .remove_sink(&bind.session_id)
            .context(format!("Cannot remove session {:?}", bind.session_id))?;

        info!("Session {:?} successfully removed!", bind.session_id);

        Ok(())
    }

    #[instrument(level = "debug")]
    fn handle_sdp(
        bind: &webrtc::signalling_protocol::BindAnswer,
        sdp: &webrtc::signalling_protocol::RTCSessionDescription,
    ) -> Result<()> {
        let manager = match MANAGER.read() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let state = match manager
            .streams
            .get(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?
            .state
            .read()
        {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let sink = state
            .pipeline
            .inner_state_as_ref()
            .sinks
            .get(&bind.session_id)
            .context(format!(
                "Session {:?} not found in producer {:?}",
                bind.producer_id, bind.producer_id
            ))?;

        let session = match sink {
            Sink::WebRTC(webrtcsink) => webrtcsink,
            _ => return Err(anyhow!("Only Sink::WebRTC accepts SDP")),
        };

        let (sdp, sdp_type) = match sdp {
            RTCSessionDescription::Answer(answer) => {
                (answer.sdp.clone(), gst_webrtc::WebRTCSDPType::Answer)
            }
            RTCSessionDescription::Offer(offer) => {
                (offer.sdp.clone(), gst_webrtc::WebRTCSDPType::Offer)
            }
        };

        let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())?;
        let sdp = gst_webrtc::WebRTCSessionDescription::new(sdp_type, sdp);
        session.handle_sdp(&sdp)
    }

    #[instrument(level = "debug")]
    fn handle_ice(
        bind: &webrtc::signalling_protocol::BindAnswer,
        sdp_m_line_index: u32,
        candidate: &str,
    ) -> Result<()> {
        let manager = match MANAGER.read() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let state = match manager
            .streams
            .get(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?
            .state
            .read()
        {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let sink = state
            .pipeline
            .inner_state_as_ref()
            .sinks
            .get(&bind.session_id)
            .context(format!(
                "Session {:?} not found in producer {:?}",
                bind.producer_id, bind.producer_id
            ))?;

        let session = match sink {
            Sink::WebRTC(webrtcsink) => webrtcsink,
            _ => return Err(anyhow!("Only Sink::WebRTC accepts SDP")),
        };

        session.handle_ice(&sdp_m_line_index, candidate)
    }
}

impl StreamManagementInterface<StreamStatus> for Manager {
    #[instrument(level = "debug")]
    fn add_stream(stream: Stream) -> Result<()> {
        let mut manager = match MANAGER.write() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        let stream_id = match stream.state.read() {
            Ok(guard) => guard.pipeline_id,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        if manager.streams.insert(stream_id, stream).is_some() {
            return Err(anyhow!("Failed adding stream {stream_id:?}"));
        }
        // manager.update_settings();

        Ok(())
    }

    #[instrument(level = "debug")]
    fn remove_stream(stream_id: &webrtc::signalling_protocol::PeerId) -> Result<()> {
        let mut manager = match MANAGER.write() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        if !manager.streams.contains_key(stream_id) {
            return Err(anyhow!("Already removed"));
        }

        manager
            .streams
            .remove(stream_id)
            .context(format!("Stream {stream_id:?} not found"))?;
        manager.update_settings();
        drop(manager);

        info!("Stream {stream_id} successfully removed!");

        Ok(())
    }

    #[instrument(level = "debug")]
    fn streams_information() -> Result<Vec<StreamStatus>> {
        let manager = match MANAGER.read() {
            Ok(guard) => guard,
            Err(error) => return Err(anyhow!("Failed locking a Mutex. Reason: {error}")),
        };

        Ok(manager
            .streams
            .values()
            .filter_map(|stream| {
                let state = match stream.state.read() {
                    Ok(guard) => guard,
                    Err(error) => {
                        error!("Failed locking a Mutex. Reason: {error}");
                        return None;
                    }
                };

                Some(StreamStatus {
                    id: state.pipeline_id,
                    running: state.pipeline.is_running(),
                    video_and_stream: state.video_and_stream_information.clone(),
                })
            })
            .collect())
    }

    #[instrument(level = "debug")]
    fn generate_uuid() -> uuid::Uuid {
        uuid::Uuid::new_v4()
    }
}
