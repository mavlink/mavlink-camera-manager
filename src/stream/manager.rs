use std::{
    collections::HashMap,
    ops::DerefMut,
    sync::{Arc, Mutex},
};

use crate::{
    settings,
    stream::{
        gst::utils::wait_for_element_state, types::CaptureConfiguration,
        webrtc::signalling_protocol::BindAnswer,
    },
    video::video_source,
};
use crate::{stream::sink::SinkInterface, video::types::VideoSourceType};
use crate::{
    stream::sink::{webrtc_sink::WebRTCSink, Sink},
    video_stream::types::VideoAndStreamInformation,
};

use anyhow::{anyhow, Context, Result};

use gst::{prelude::*, traits::ElementExt};
use tracing::*;

use super::{
    pipeline::PipelineGstreamerInterface,
    types::StreamStatus,
    webrtc::{
        self,
        signalling_protocol::RTCSessionDescription,
        signalling_server::{StreamManagementInterface, WebRTCSessionManagementInterface},
        webrtcbin_interface::WebRTCBinInterface,
    },
    Stream,
};

#[derive(Default)]
pub struct Manager {
    streams: HashMap<uuid::Uuid, Stream>,
}

lazy_static! {
    static ref MANAGER: Arc<Mutex<Manager>> = Arc::new(Mutex::new(Manager::default()));
}

impl Manager {
    #[instrument(level = "debug", skip(self))]
    fn update_settings(&self) {
        let video_and_stream_informations = self
            .streams
            .values()
            .map(|stream| stream.video_and_stream_information.clone())
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

#[instrument(level = "debug")]
pub fn start_default() {
    MANAGER.as_ref().lock().unwrap().streams.clear();

    let mut streams = settings::manager::streams();

    // Update all local video sources to make sure that they are available
    let mut candidates = video_source::cameras_available();
    update_devices(&mut streams, &mut candidates);

    // Remove all invalid video_sources
    let streams: Vec<VideoAndStreamInformation> = streams
        .into_iter()
        .filter(|stream| stream.video_source.inner().is_valid())
        .collect();

    debug!("Streams: {streams:#?}");

    for stream in streams {
        add_stream_and_start(stream).unwrap_or_else(|error| {
            error!("Not possible to start stream: {error:?}");
        });
    }
}

#[instrument(level = "debug")]
fn update_devices(
    streams: &mut Vec<VideoAndStreamInformation>,
    candidates: &mut Vec<VideoSourceType>,
) {
    for stream in streams {
        let VideoSourceType::Local(source) = &mut stream.video_source else {
            continue
        };
        let CaptureConfiguration::Video(capture_configuration) = &stream.stream_information.configuration else {
            continue
        };

        match source.try_identify_device(capture_configuration, candidates) {
            Ok(Some(candidate_source_string)) => {
                let Some((idx, candidate)) = candidates.iter().enumerate().find_map(|(idx, candidate)| {
                        (candidate.inner().source_string() == candidate_source_string)
                            .then_some((idx, candidate))
                    }) else {
                        error!("CRITICAL: The device was identified as {candidate_source_string:?}, but it is not the candidates list"); // This shouldn't ever be reachable, otherwise the above logic is flawed
                        continue
                    };

                let VideoSourceType::Local(camera) = candidate else {
                        error!("CRITICAL: The device was identified as {candidate_source_string:?}, but it is not a Local device"); // This shouldn't ever be reachable, otherwise the above logic is flawed
                        continue
                    };
                *source = camera.clone();
                // Only remove the candidate from the list after using it, avoiding the wrong but possible logic from the CRITICAL erros branches above, the additional cost is this clone
                candidates.remove(idx);
            }
            Err(reason) => {
                // Invalidate the device
                source.device_path = "".into();
                warn!("Device {source:?} was invalidated. Reason: {reason:?}");
            }
            _ => (),
        }
    }
}

#[instrument(level = "debug")]
pub fn streams() -> Vec<StreamStatus> {
    Manager::streams_information()
}

#[instrument(level = "debug")]
pub fn get_first_sdp_from_source(source: String) -> Result<gst_sdp::SDPMessage> {
    let Some(result) = MANAGER
        .lock()
        .unwrap()
        .streams
        .values()
        .find_map(|stream| {
            if stream.video_and_stream_information.video_source.inner().source_string() == source {
                stream
                    .pipeline
                    .inner_state_as_ref()
                    .sinks
                    .values()
                    .find_map(|sink| {
                        sink.get_sdp().ok()
                    })
            } else {
                None
            }
        }) else {
            return Err(anyhow!("Failed to find any valid sdp for souce {source:?}"));
        };
    Ok(result)
}

#[instrument(level = "debug")]
pub fn get_jpeg_thumbnail_from_source(
    source: String,
    quality: u8,
    target_height: Option<u32>,
) -> Option<Result<Vec<u8>>> {
    MANAGER.lock().unwrap().streams.values().find_map(|stream| {
        if stream
            .video_and_stream_information
            .video_source
            .inner()
            .source_string()
            == source
        {
            stream
                .pipeline
                .inner_state_as_ref()
                .sinks
                .values()
                .find_map(|sink| match sink {
                    Sink::Image(image_sink) => {
                        Some(image_sink.make_jpeg_thumbnail_from_last_frame(quality, target_height))
                    }
                    _ => None,
                })
        } else {
            None
        }
    })
}

#[instrument(level = "debug")]
pub fn add_stream_and_start(video_and_stream_information: VideoAndStreamInformation) -> Result<()> {
    let manager = MANAGER.as_ref().lock().unwrap();
    for stream in manager.streams.values() {
        stream
            .video_and_stream_information
            .conflicts_with(&video_and_stream_information)?;
    }
    drop(manager);

    let stream = Stream::try_new(&video_and_stream_information)?;
    Manager::add_stream(stream)?;

    Ok(())
}

#[instrument(level = "debug")]
pub fn remove_stream_by_name(stream_name: &str) -> Result<()> {
    let manager = MANAGER.as_ref().lock().unwrap();
    if let Some(stream_id) = &manager.streams.iter().find_map(|(id, stream)| {
        if stream.video_and_stream_information.name == *stream_name {
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
        let mut guard = MANAGER.lock().unwrap();
        let manager = guard.deref_mut();

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
        stream.pipeline.add_sink(sink)?;
        debug!("WebRTC session created: {session_id:?}");

        Ok(session_id)
    }

    #[instrument(level = "debug")]
    fn remove_session(
        bind: &webrtc::signalling_protocol::BindAnswer,
        _reason: String,
    ) -> Result<()> {
        let mut manager = MANAGER.lock().unwrap();

        let stream = manager
            .streams
            .get_mut(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?;

        stream
            .pipeline
            .inner_state_mut()
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
        let manager = MANAGER.lock().unwrap();

        let sink = manager
            .streams
            .get(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?
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
        let manager = MANAGER.lock().unwrap();

        let sink = manager
            .streams
            .get(&bind.producer_id)
            .context(format!("Producer {:?} not found", bind.producer_id))?
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
        let mut manager = MANAGER.lock().unwrap();

        let stream_id = stream.id;
        if manager.streams.insert(stream_id, stream).is_some() {
            return Err(anyhow!("Failed adding stream {stream_id:?}"));
        }
        manager.update_settings();

        Ok(())
    }

    #[instrument(level = "debug")]
    fn remove_stream(stream_id: &webrtc::signalling_protocol::PeerId) -> Result<()> {
        let mut manager = MANAGER.lock().unwrap();

        if !manager.streams.contains_key(stream_id) {
            return Err(anyhow!("Already removed"));
        }

        let mut stream = manager
            .streams
            .remove(stream_id)
            .context(format!("Stream {stream_id:?} not found"))?;
        manager.update_settings();
        drop(manager);

        let pipeline = &stream.pipeline.inner_state_as_ref().pipeline;
        let pipeline_id = stream_id;
        if let Err(error) = pipeline.post_message(gst::message::Eos::new()) {
            error!("Failed posting Eos message into Pipeline' bus. Reason: {error:?}");
        }

        if let Err(error) = stream
            .pipeline
            .inner_state_as_ref()
            .pipeline
            .set_state(gst::State::Null)
        {
            error!("Failed setting Pipeline {pipeline_id:?} state to Null. Reason: {error:?}");
        }
        if let Err(error) = wait_for_element_state(
            pipeline.upcast_ref::<gst::Element>(),
            gst::State::Null,
            100,
            10,
        ) {
            let _ = pipeline.set_state(gst::State::Null);
            error!("Failed setting Pipeline {pipeline_id:?} state to Null. Reason: {error:?}");
        }

        // Remove all Sinks
        let sink_ids = &stream
            .pipeline
            .inner_state_as_ref()
            .sinks
            .keys()
            .cloned()
            .collect::<Vec<uuid::Uuid>>();
        for sink_id in sink_ids {
            if let Err(error) = stream.pipeline.remove_sink(sink_id) {
                warn!(
                    "Failed unlinking Sink {sink_id:?} from Pipeline {pipeline_id:?}. Reason: {error:?}"
                );
            }
        }

        info!("Stream {stream_id} successfully removed!");

        Ok(())
    }

    #[instrument(level = "debug")]
    fn streams_information() -> Vec<StreamStatus> {
        MANAGER
            .lock()
            .unwrap()
            .streams
            .values()
            .map(|stream| StreamStatus {
                id: stream.id,
                running: stream.pipeline.is_running(),
                video_and_stream: stream.video_and_stream_information.clone(),
            })
            .collect()
    }

    #[instrument(level = "debug")]
    fn generate_uuid() -> uuid::Uuid {
        uuid::Uuid::new_v4()
    }
}
