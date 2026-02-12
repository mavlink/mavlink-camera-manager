use std::{collections::HashMap, sync::Arc};

use anyhow::{anyhow, Context, Error, Result};
use cached::proc_macro::cached;
use futures::stream::StreamExt;
use gst::{
    prelude::{ElementExtManual, GstBinExtManual},
    DebugGraphDetails,
};
use tokio::sync::RwLock;
use tracing::*;

use mcm_api::v1::{
    signalling,
    stream::{CaptureConfiguration, StreamStatus, StreamStatusState, VideoAndStreamInformation},
    video::VideoSourceType,
};

use crate::{
    settings,
    stream::sink::{rtp_queue_max_time_ns, webrtc_sink::WebRTCSink, Sink, SinkInterface},
    video::{types::VideoSourceTypeExt, video_source, video_source_local::VideoSourceLocalExt},
    video_stream::types::VideoAndStreamInformationExt,
};

use super::{pipeline::PipelineGstreamerInterface, Stream};

type ClonableResult<T> = Result<T, Arc<Error>>;

#[derive(Default)]
pub struct Manager {
    streams: HashMap<uuid::Uuid, Stream>,
}

lazy_static! {
    static ref MANAGER: Arc<RwLock<Manager>> = Default::default();
}

impl Manager {
    #[instrument(level = "debug", skip(self))]
    async fn update_settings(&self) {
        let mut video_and_stream_informations = vec![];
        for stream in self.streams.values() {
            video_and_stream_informations
                .push(stream.video_and_stream_information.read().await.clone())
        }

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

pub async fn remove_all_streams() -> Result<()> {
    let keys = {
        let mut ids = vec![];

        MANAGER.read().await.streams.keys().for_each(|id| {
            ids.push(*id);
        });

        ids
    };

    for stream_id in keys.iter() {
        if let Err(error) = Manager::remove_stream(stream_id, false).await {
            warn!("Failed removing stream {stream_id:?}: {error:?}")
        }
    }

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
    remove_all_streams().await?;

    // Update all local video sources to make sure that they are available
    let mut candidates = video_source::cameras_available().await;
    update_devices(&mut streams, &mut candidates, true).await;

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
pub async fn update_devices(
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

        match source
            .try_identify_device(capture_configuration, candidates)
            .await
        {
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
pub async fn streams() -> Result<Vec<StreamStatus>> {
    Manager::streams_information().await
}

#[instrument(level = "debug")]
#[cached(time = 1)]
pub async fn get_first_sdp_from_source(source: String) -> ClonableResult<gst_sdp::SDPMessage> {
    let manager = MANAGER.read().await;

    let result = futures::stream::iter(manager.streams.values())
        .filter_map(|stream| {
            let source = &source;

            let future = async move {
                let state_guard = stream.state.read().await;

                let state_ref = state_guard.as_ref()?;

                if state_ref
                    .video_and_stream_information
                    .read()
                    .await
                    .video_source
                    .inner()
                    .source_string()
                    .eq(source)
                {
                    let Some(pipeline) = &state_ref.pipeline else {
                        return None;
                    };

                    return pipeline
                        .inner_state_as_ref()
                        .sinks
                        .values()
                        .find_map(|sink| sink.get_sdp().ok());
                }

                None
            };
            Box::pin(future)
        })
        .next()
        .await
        .context(format!("Failed to find any valid sdp for souce {source:?}"))
        .map_err(Arc::new);

    drop(manager);

    result
}

#[instrument(level = "debug")]
#[cached(time = 1)]
pub async fn get_jpeg_thumbnail_from_source(
    source: String,
    quality: u8,
    target_height: Option<u32>,
) -> Option<ClonableResult<Vec<u8>>> {
    // Tokio runtime create workers within OS threads. These workers receive tasks to run.
    // Tokio runtime uses a non-preemptive task manager, so it can only switch tasks when
    // they yield, which might happen in the .await parts of the code, or when the task
    // finishes.
    // When a blocking task is running, all other tasks in the same pool will also block.
    // If one of the other tasks happens to have acquired a Mutex (like here, most of our
    // endpoints asks for something that depends on the MANAGER, which sits behind a
    // Mutex), then all subsequent tasks waiting for that Mutex to be available will
    // be blocked until that blocking task finishes. This is not the case here, but by
    // chance it can also be the case of that blocking task be waiting for that same
    // MANAGER Mutex, and then it will be a deadlock. Another deadlock could happen if
    // the blocking task never finishes. To solve this, a naive approach would be to
    // use a timeout, but this timeout could be running in the same blocked pool, and
    // therefore, also be blocked.
    // A reliable solution is to spawn a new OS thread for each blocking task, and
    // if we need async, we just create another single-threaded tokio runtime.
    // Differently from using Tokio's spawn_blocking (plus block_on to use async), this
    // method will garantee that every request will not interfer with other running tasks,
    // as those dealing with Mutexes, or other requests of the same nature. The drawnback
    // is to have the overhead of a new OS thread plus a new Tokio runtime for each request
    // of this kind.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        crate::helper::threads::lower_thread_priority();
        tokio::runtime::Builder::new_current_thread()
            .on_thread_start(|| debug!("Thread started"))
            .on_thread_stop(|| debug!("Thread stopped"))
            .thread_name_fn(|| {
                static ATOMIC_ID: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let id = ATOMIC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("Thumbnailer-{id}")
            })
            .enable_time()
            .build()
            .expect("Failed building a new tokio runtime")
            .block_on(async move {
                let manager = MANAGER.read().await;

                // Find the matching stream
                let stream = manager.streams.values().find(|stream| {
                    let Ok(guard) = stream.video_and_stream_information.try_read() else {
                        return false;
                    };
                    guard.video_source.inner().source_string() == source
                });

                let Some(stream) = stream else {
                    let _ = tx.send(None);
                    return;
                };

                // If the stream is idle (lazy-suspended), temporarily wake it
                // and wait until data is flowing (position advancing).
                let was_idle = stream.idle.load(std::sync::atomic::Ordering::Relaxed);
                if was_idle {
                    stream
                        .idle
                        .store(false, std::sync::atomic::Ordering::Relaxed);

                    let deadline =
                        tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
                    let mut last_position: Option<gst::ClockTime> = None;
                    loop {
                        if tokio::time::Instant::now() > deadline {
                            debug!("Pipeline did not resume in time for thumbnail");
                            let _ = tx.send(Some(Err(Arc::new(anyhow!(
                                "Pipeline did not resume in time for thumbnail"
                            )))));
                            return;
                        }

                        let flowing = stream.state.read().await.as_ref().is_some_and(|st| {
                            st.pipeline.as_ref().is_some_and(|p| {
                                let pipeline = &p.inner_state_as_ref().pipeline;
                                if pipeline.current_state() != gst::State::Playing {
                                    return false;
                                }
                                if let Some(pos) = pipeline.query_position::<gst::ClockTime>() {
                                    let advanced = last_position.map_or(false, |prev| pos > prev);
                                    last_position = Some(pos);
                                    advanced
                                } else {
                                    false
                                }
                            })
                        });
                        if flowing {
                            break;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }

                let state_guard = stream.state.read().await;
                let res = async {
                    let state_ref = state_guard.as_ref()?;

                    let pipeline = state_ref.pipeline.as_ref()?;

                    let sinks = pipeline.inner_state_as_ref().sinks.values();

                    let sink = sinks
                        .into_iter()
                        .find(|sink| matches!(sink, Sink::Image(_)))?;

                    let Sink::Image(image_sink) = sink else {
                        return None;
                    };

                    Some(
                        image_sink
                            .make_jpeg_thumbnail_from_last_frame(quality, target_height)
                            .await
                            .map_err(Arc::new),
                    )
                }
                .await;

                let _ = tx.send(res);
            });
    });

    match rx.await {
        Ok(res) => res,
        Err(error) => Some(Err(Arc::new(anyhow!(error.to_string())))),
    }
}

#[instrument(level = "debug", skip_all)]
pub async fn add_stream_and_start(
    video_and_stream_information: VideoAndStreamInformation,
) -> Result<()> {
    // Check if source is blocked
    let source_string = video_and_stream_information
        .video_source
        .inner()
        .source_string();
    if is_source_blocked(&source_string) {
        return Err(anyhow!(
            "Source {source_string:?} needs to be unblocked to be used"
        ));
    }

    {
        let manager = MANAGER.read().await;
        for stream in manager.streams.values() {
            stream
                .video_and_stream_information
                .read()
                .await
                .conflicts_with(&video_and_stream_information)?;
        }
    }

    let stream = Stream::try_new(&video_and_stream_information).await?;
    Manager::add_stream(stream).await?;

    Ok(())
}

#[instrument(level = "debug")]
async fn get_stream_id_from_name(stream_name: &str) -> Result<uuid::Uuid> {
    let manager = MANAGER.read().await;

    let stream_id = futures::stream::iter(&manager.streams)
        .filter_map(|(id, stream)| {
            let future = async move {
                let video_and_stream_information = stream.video_and_stream_information.read().await;

                video_and_stream_information
                    .name
                    .eq(stream_name)
                    .then_some(*id)
            };
            Box::pin(future)
        })
        .next()
        .await
        .context(format!("Stream named {stream_name:?} not found"));

    drop(manager);

    stream_id
}

#[instrument(level = "debug")]
pub async fn remove_stream_by_name(stream_name: &str) -> Result<()> {
    let stream_id = get_stream_id_from_name(stream_name).await?;

    Manager::remove_stream(&stream_id, true).await?;

    Ok(())
}

#[instrument(level = "debug")]
pub async fn block_source(source_string: &str) -> Result<()> {
    // Add to blocked list
    settings::manager::add_blocked_source(source_string);

    // Remove all streams using this source
    let streams_to_remove = {
        let manager = MANAGER.read().await;
        let mut ids = vec![];

        for (id, stream) in manager.streams.iter() {
            let video_and_stream_info = stream.video_and_stream_information.read().await;
            if video_and_stream_info.video_source.inner().source_string() == source_string {
                ids.push(*id);
            }
        }

        ids
    };

    for stream_id in streams_to_remove {
        if let Err(error) = Manager::remove_stream(&stream_id, true).await {
            warn!("Failed removing stream {stream_id:?} while blocking source: {error:?}");
        }
    }

    Ok(())
}

#[instrument(level = "debug")]
pub async fn unblock_source(source_string: &str) -> Result<()> {
    settings::manager::remove_blocked_source(source_string);
    Ok(())
}

#[instrument(level = "debug")]
pub async fn clear_blocked_sources() {
    settings::manager::clear_blocked_sources();
}

#[instrument(level = "debug")]
pub fn blocked_sources() -> Vec<String> {
    settings::manager::blocked_sources()
}

#[instrument(level = "debug")]
pub fn is_source_blocked(source_string: &str) -> bool {
    settings::manager::is_source_blocked(source_string)
}

impl Manager {
    #[instrument(level = "debug", skip(sender))]
    pub async fn add_session(
        bind: &signalling::BindOffer,
        sender: tokio::sync::mpsc::UnboundedSender<Result<signalling::Message>>,
    ) -> Result<signalling::SessionId> {
        use std::sync::atomic::Ordering;

        let producer_id = bind.producer_id;

        // If the pipeline is idle (lazy mode), wake it up and wait until
        // data is actually flowing (position advancing) so the WebRTC sink
        // can negotiate successfully.
        {
            let manager = MANAGER.read().await;
            let stream = manager.streams.get(&producer_id).context(format!(
                "Cannot find any stream with producer {producer_id:?}"
            ))?;
            if stream.idle.load(Ordering::Relaxed) {
                stream.idle.store(false, Ordering::Relaxed);
                drop(manager);

                let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
                let mut last_position: Option<gst::ClockTime> = None;
                loop {
                    {
                        let mgr = MANAGER.read().await;
                        if let Some(s) = mgr.streams.get(&producer_id) {
                            let flowing = s.state.read().await.as_ref().is_some_and(|st| {
                                st.pipeline.as_ref().is_some_and(|p| {
                                    let pipeline = &p.inner_state_as_ref().pipeline;
                                    if pipeline.current_state() != gst::State::Playing {
                                        return false;
                                    }
                                    if let Some(pos) = pipeline.query_position::<gst::ClockTime>() {
                                        let advanced =
                                            last_position.map_or(false, |prev| pos > prev);
                                        last_position = Some(pos);
                                        advanced
                                    } else {
                                        false
                                    }
                                })
                            });
                            if flowing {
                                break;
                            }
                        } else {
                            return Err(anyhow::anyhow!(
                                "Stream {producer_id:?} removed while waking"
                            ));
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(anyhow::anyhow!(
                            "Lazy pipeline for {producer_id:?} did not resume in time"
                        ));
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
        }

        let mut manager = MANAGER.write().await;

        let consumer_id = bind.consumer_id;
        let session_id = Self::generate_uuid(None);

        let stream = manager.streams.get_mut(&producer_id).context(format!(
            "Cannot find any stream with producer {producer_id:?}"
        ))?;

        let consumer_count = stream.consumer_count.clone();
        let idle = stream.idle.clone();

        let bind = signalling::BindAnswer {
            producer_id,
            consumer_id,
            session_id,
        };

        let queue_time_ns = {
            let info = stream.video_and_stream_information.read().await;
            rtp_queue_max_time_ns(&info)
        };

        let sink = Sink::WebRTC(WebRTCSink::try_new(bind, sender, queue_time_ns)?);

        let mut state_guard = stream.state.write().await;

        let state_mut = state_guard.as_mut().context("Stream without State")?;

        state_mut
            .pipeline
            .as_mut()
            .context("No Pipeline")?
            .add_sink(sink)
            .await?;

        consumer_count.fetch_add(1, Ordering::Relaxed);
        idle.store(false, Ordering::Relaxed);

        debug!("WebRTC session created: {session_id:?}");

        Ok(session_id)
    }

    #[instrument(level = "debug")]
    pub async fn remove_session(bind: &signalling::BindAnswer, _reason: String) -> Result<()> {
        use std::sync::atomic::Ordering;

        let mut manager = MANAGER.write().await;

        if !manager.streams.contains_key(&bind.producer_id) {
            debug!(
                "Tried to remove session {:?}, but it was already removed",
                bind.producer_id
            );
            return Ok(());
        }

        let stream = manager
            .streams
            .get_mut(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?;

        let consumer_count = stream.consumer_count.clone();

        let mut state_guard = stream.state.write().await;

        let state_mut = state_guard.as_mut().context("Stream without State")?;

        match state_mut
            .pipeline
            .as_mut()
            .context("No Pipeline")?
            .remove_sink(&bind.session_id)
            .await
        {
            Ok(()) => {
                consumer_count.fetch_sub(1, Ordering::Relaxed);
                info!("Session {:?} successfully removed!", bind.session_id);
            }
            Err(error) => {
                debug!(
                    "Session {:?} already removed or not found: {error}",
                    bind.session_id
                );
            }
        }

        Ok(())
    }

    #[instrument(level = "debug")]
    pub async fn handle_sdp(
        bind: &signalling::BindAnswer,
        sdp: &signalling::RTCSessionDescription,
    ) -> Result<()> {
        let manager = MANAGER.read().await;

        let stream = manager
            .streams
            .get(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?;

        let state_guard = stream.state.read().await;

        let state_ref = state_guard.as_ref().context("Stream without State")?;

        let sink = state_ref
            .pipeline
            .as_ref()
            .context("No Pipeline")?
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
            signalling::RTCSessionDescription::Answer(answer) => {
                (answer.sdp.clone(), gst_webrtc::WebRTCSDPType::Answer)
            }
            signalling::RTCSessionDescription::Offer(offer) => {
                (offer.sdp.clone(), gst_webrtc::WebRTCSDPType::Offer)
            }
        };

        let sdp = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())?;
        let sdp = gst_webrtc::WebRTCSessionDescription::new(sdp_type, sdp);
        session.handle_sdp(&sdp)
    }

    #[instrument(level = "debug")]
    pub async fn handle_ice(
        bind: &signalling::BindAnswer,
        sdp_m_line_index: u32,
        candidate: &str,
    ) -> Result<()> {
        let manager = MANAGER.read().await;

        let stream = manager
            .streams
            .get(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?;

        let state_guard = stream.state.read().await;

        let state_ref = state_guard.as_ref().context("Stream without State")?;

        let sink = state_ref
            .pipeline
            .as_ref()
            .context("No Pipeline")?
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

    #[instrument(level = "debug", skip_all)]
    pub async fn add_stream(stream: Stream) -> Result<()> {
        let mut manager = MANAGER.write().await;

        let stream_id = *stream.pipeline_id;

        if manager.streams.insert(stream_id, stream).is_some() {
            return Err(anyhow!("Failed adding stream {stream_id:?}"));
        }
        manager.update_settings().await;

        info!("Stream {stream_id} successfully added!");

        Ok(())
    }

    #[instrument(level = "debug")]
    pub async fn remove_stream(stream_id: &signalling::PeerId, rewrite_config: bool) -> Result<()> {
        let mut manager = MANAGER.write().await;

        if !manager.streams.contains_key(stream_id) {
            return Err(anyhow!("Already removed"));
        }

        manager
            .streams
            .remove(stream_id)
            .context(format!("Stream {stream_id:?} not found"))?;

        if rewrite_config {
            manager.update_settings().await;
        }

        info!("Stream {stream_id} successfully removed!");

        Ok(())
    }

    #[instrument(level = "debug")]
    pub async fn streams_information() -> Result<Vec<StreamStatus>> {
        use std::sync::atomic::Ordering;

        let manager = MANAGER.read().await;

        let status = futures::stream::iter(manager.streams.values())
            .filter_map(|stream| async move {
                let state_guard = stream.state.read().await;

                let id = *stream.pipeline_id;
                let running = state_guard
                    .as_ref()
                    .map(|state| {
                        state
                            .pipeline
                            .as_ref()
                            .map(super::pipeline::Pipeline::is_running)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();

                let is_idle = stream.idle.load(Ordering::Relaxed);

                let state = if is_idle {
                    StreamStatusState::Idle
                } else if running {
                    StreamStatusState::Running
                } else {
                    StreamStatusState::Stopped
                };

                let running = running && !is_idle;

                let error = stream
                    .error
                    .read()
                    .await
                    .as_ref()
                    .err()
                    .map(|error| error.to_string());

                let video_and_stream = stream.video_and_stream_information.read().await.clone();

                let mavlink = state_guard
                    .as_ref()
                    .map(|state| state.mavlink_camera.as_ref().map(|m| m.into()))
                    .unwrap_or_default();

                Some(StreamStatus {
                    id,
                    running,
                    state,
                    error,
                    video_and_stream,
                    mavlink,
                })
            })
            .collect()
            .await;

        Ok(status)
    }

    #[instrument(level = "debug")]
    pub fn generate_uuid(data: Option<&str>) -> uuid::Uuid {
        if let Some(data) = data {
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, data.as_bytes())
        } else {
            uuid::Uuid::new_v4()
        }
    }

    pub async fn get_stream_dot_by_id(id: &uuid::Uuid) -> Option<(String, Vec<String>)> {
        let manager = MANAGER.read().await;
        let stream = manager.streams.get(id)?;

        let state_guard = stream.state.read().await;
        let state = state_guard.as_ref()?;

        let pipeline = state.pipeline.as_ref()?;

        let dot_main = pipeline
            .inner_state_as_ref()
            .pipeline
            .debug_to_dot_data(DebugGraphDetails::all())
            .to_string();

        let dot_children = pipeline
            .inner_state_as_ref()
            .sinks
            .values()
            .filter_map(|sink| {
                sink.pipeline().map(|pipeline| {
                    pipeline
                        .debug_to_dot_data(DebugGraphDetails::all())
                        .to_string()
                })
            })
            .collect::<Vec<String>>();

        Some((dot_main, dot_children))
    }
}
