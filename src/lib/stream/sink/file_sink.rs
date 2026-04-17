use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use gst::prelude::*;
use tokio::sync::RwLock;
use tracing::*;

use crate::{
    cli,
    stream::{pipeline::runner::PipelineRunner, types::CaptureConfiguration},
    video::types::VideoEncodeType,
    video_stream::types::VideoAndStreamInformation,
};

use super::{link_sink_to_tee, unlink_sink_from_tee, SinkInterface};

/// Recording state for the file sink
#[derive(Debug, Clone, PartialEq, Default)]
pub enum RecordingState {
    #[default]
    Idle,
    Recording {
        start_time: std::time::Instant,
        file_path: String,
    },
    Stopping,
    Error(String),
}

#[derive(Debug)]
pub struct FileSink {
    sink_id: Arc<uuid::Uuid>,
    pipeline: gst::Pipeline,
    proxysink: gst::Element,
    _proxysrc: gst::Element,
    _parse_element: gst::Element,
    _muxer: gst::Element,
    _filesink: gst::Element,
    tee_src_pad: Option<gst::Pad>,
    pipeline_runner: PipelineRunner,
    recording_state: Arc<RwLock<RecordingState>>,
    file_path: String,
    stream_name: String,
    encoding: VideoEncodeType,
}

impl SinkInterface for FileSink {
    #[instrument(level = "debug", skip(self, pipeline))]
    fn link(
        &mut self,
        pipeline: &gst::Pipeline,
        pipeline_id: &Arc<uuid::Uuid>,
        tee_src_pad: gst::Pad,
    ) -> Result<()> {
        if self.tee_src_pad.is_some() {
            return Err(anyhow!(
                "Tee's src pad from Sink {:?} has already been configured",
                self.get_id()
            ));
        }
        self.tee_src_pad.replace(tee_src_pad);
        let Some(tee_src_pad) = &self.tee_src_pad else {
            unreachable!()
        };

        let elements = &[&self.proxysink];
        link_sink_to_tee(tee_src_pad, pipeline, elements)?;

        Ok(())
    }

    #[instrument(level = "debug", skip(self, pipeline))]
    fn unlink(&self, pipeline: &gst::Pipeline, pipeline_id: &Arc<uuid::Uuid>) -> Result<()> {
        let Some(tee_src_pad) = &self.tee_src_pad else {
            warn!("Tried to unlink Sink from a pipeline without a Tee src pad.");
            return Ok(());
        };

        let elements = &[&self.proxysink];
        unlink_sink_from_tee(tee_src_pad, pipeline, elements)?;

        if let Err(error) = self.pipeline.set_state(::gst::State::Null) {
            warn!("Failed setting sink Pipeline state to Null: {error:?}");
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn get_id(&self) -> Arc<uuid::Uuid> {
        self.sink_id.clone()
    }

    #[instrument(level = "trace", skip(self))]
    fn get_sdp(&self) -> Result<gst_sdp::SDPMessage> {
        Err(anyhow!(
            "Not available. Reason: File Sink doesn't provide endpoints"
        ))
    }

    #[instrument(level = "debug", skip(self))]
    fn start(&self) -> Result<()> {
        self.pipeline_runner.start()?;

        // Ask the upstream encoder for a fresh IDR. Without this, a dynamic
        // recording start latches onto the tee mid-GOP; the file then begins
        // with P-frames that reference an I-frame we never write, so the
        // container parses but every decoder produces garbage (mpv refuses
        // to play it, VLC shows corrupted frames).
        //
        // The drop probe installed in `try_new` on `proxysrc`'s src pad
        // discards delta buffers until this IDR propagates through, so the
        // first sample written to `mp4mux` is always a keyframe with SPS/PPS
        // (courtesy of `h264parse config-interval=-1`).
        //
        // Same idiom used by `rtsp_sink` (first client connect) and
        // `webrtc_sink` (session connect). We send on the tee src pad we
        // were linked to (not on `proxysink.sink_pad`) because the event
        // must traverse the upstream peer of that link to reach the tee
        // and, from there, the encoder; the proxysink element itself does
        // not forward upstream events across the proxy pair.
        if let Some(tee_src_pad) = self.tee_src_pad.as_ref() {
            let event = gst_video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            if !tee_src_pad.send_event(event) {
                warn!("FileSink: UpstreamForceKeyUnitEvent not handled; first frame may not be a keyframe");
            }
        } else {
            warn!("FileSink: tee_src_pad not set; cannot request keyframe");
        }

        Ok(())
    }

    #[instrument(level = "debug", skip(self))]
    fn eos(&self) {
        // We need two things to guarantee a finalised mp4 file:
        //
        // 1. Push an EOS *event* downstream from the head of this internal
        //    pipeline (not a bus *message*, not `pipeline.send_event`).
        //    Only an EOS event makes mp4mux rewrite the moov with the real
        //    sample table. We push directly from `proxysrc`'s src pad
        //    because `proxysrc` is not a GstBaseSrc and does not translate
        //    `pipeline.send_event(Eos)` into a downstream EOS on every
        //    codec path (h265 notably stalled in that case).
        // 2. Wait until the EOS reaches `filesink`'s sink pad, at which
        //    point the mux has finished writing the moov and filesink has
        //    flushed. We cannot wait on the pipeline bus because the
        //    PipelineRunner's `bus_watcher_task` consumes messages from
        //    it; instead we attach a one-shot pad probe on `filesink`'s
        //    sink pad.
        let filesink_sink_pad = match self._filesink.static_pad("sink") {
            Some(pad) => pad,
            None => {
                error!("FileSink: filesink has no sink pad, cannot arm EOS probe");
                return;
            }
        };
        let proxysrc_src_pad = match self._proxysrc.static_pad("src") {
            Some(pad) => pad,
            None => {
                error!("FileSink: proxysrc has no src pad, cannot inject EOS");
                return;
            }
        };

        let (eos_tx, eos_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let probe_id =
            filesink_sink_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
                if let Some(gst::PadProbeData::Event(event)) = &info.data {
                    if event.type_() == gst::EventType::Eos {
                        let _ = eos_tx.try_send(());
                        return gst::PadProbeReturn::Remove;
                    }
                }
                gst::PadProbeReturn::Ok
            });

        if probe_id.is_none() {
            error!("FileSink: failed to arm EOS probe on filesink sink pad");
            return;
        }

        if !proxysrc_src_pad.push_event(gst::event::Eos::new()) {
            error!("FileSink: proxysrc rejected EOS event");
            if let Some(id) = probe_id {
                filesink_sink_pad.remove_probe(id);
            }
            return;
        }

        // 2 s is above the worst-case mp4mux finalise (flush a <=1 s
        // fragment and rewrite the moov). We must stay well under the
        // MAVLink stop-capture ACK deadline so we don't hang the command
        // handler on a stuck pipeline.
        match eos_rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(()) => {}
            Err(error) => {
                warn!("FileSink: EOS did not reach filesink in time ({error}); file may be unfinalised");
                if let Some(id) = probe_id {
                    filesink_sink_pad.remove_probe(id);
                }
            }
        }
    }

    fn pipeline(&self) -> Option<&gst::Pipeline> {
        Some(&self.pipeline)
    }
}

impl FileSink {
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        sink_id: Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
        recording_path: Option<String>,
    ) -> Result<Self> {
        let stream_name = video_and_stream_information.name.clone();

        let encoding = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(video_configuration) => video_configuration.encode.clone(),
            CaptureConfiguration::Redirect(_) => {
                return Err(anyhow!(
                    "FileSink cannot be created for Redirect CaptureConfiguration"
                ));
            }
        };

        // Validate encoding type
        if !matches!(
            encoding,
            VideoEncodeType::H264 | VideoEncodeType::H265 | VideoEncodeType::Mjpg
        ) {
            return Err(anyhow!(
                "Unsupported video encoding for FileSink: {encoding:?}. Only H264, H265 and MJPG are supported."
            ));
        }

        // Create a pair of proxies. The proxysink will be used in the source's pipeline,
        // while the proxysrc will be used in this sink's pipeline
        let proxysink = gst::ElementFactory::make("proxysink").build()?;
        let _proxysrc = gst::ElementFactory::make("proxysrc")
            .property("proxysink", &proxysink)
            .build()?;

        // Drop every non-keyframe buffer at the head of the sub-pipeline
        // until the first IDR passes through, then the probe removes itself.
        // Paired with the UpstreamForceKeyUnitEvent sent from `start()`,
        // this guarantees the first sample muxed into the MP4 is always a
        // keyframe (with SPS/PPS prepended by `h264parse config-interval=-1`).
        //
        // Without this, a dynamic recording start latches onto the tee
        // mid-GOP and writes P-frames that reference an I-frame we never
        // captured: the container demuxes, but every decoder produces
        // garbage (mpv refuses to play, VLC shows corrupted frames).
        if let Some(proxysrc_src_pad) = _proxysrc.static_pad("src") {
            proxysrc_src_pad.add_probe(gst::PadProbeType::BUFFER, |_pad, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data {
                    if buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                        return gst::PadProbeReturn::Drop;
                    }
                }
                gst::PadProbeReturn::Remove
            });
        } else {
            warn!("FileSink: proxysrc has no src pad; cannot arm keyframe-gate probe");
        }

        // Configure proxysrc's queue, skips if fails
        match _proxysrc.downcast_ref::<gst::Bin>() {
            Some(bin) => {
                let elements = bin.children();
                match elements
                    .iter()
                    .find(|element| element.name().starts_with("queue"))
                {
                    Some(element) => {
                        element.set_property("silent", true);
                        element.set_property("flush-on-eos", true);
                        element.set_property("max-size-buffers", 0u32);
                        element.set_property("max-size-bytes", 0u32);
                        element.set_property("max-size-time", 5_000_000_000u64);
                    }
                    None => {
                        warn!("Failed to customize proxysrc's queue: Failed to find queue in proxysrc");
                    }
                }
            }
            None => {
                warn!("Failed to customize proxysrc's queue: Failed to downcast element to bin")
            }
        }

        // Create parser element based on encoding
        let _parse_element = match &encoding {
            VideoEncodeType::H264 => gst::ElementFactory::make("h264parse")
                .property("config-interval", -1i32)
                .build()?,
            VideoEncodeType::H265 => gst::ElementFactory::make("h265parse")
                .property("config-interval", -1i32)
                .build()?,
            VideoEncodeType::Mjpg => gst::ElementFactory::make("jpegparse").build()?,
            _ => unreachable!("Encoding type already validated"),
        };

        let recordings_dir = recording_path.unwrap_or_else(cli::manager::recording_path);
        if let Err(error) = std::fs::create_dir_all(&recordings_dir) {
            return Err(anyhow!(
                "Failed to create recording directory {recordings_dir}: {error}"
            ));
        }

        let sanitized_stream_name = sanitize_filename(&stream_name);
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let file_path = format!("{recordings_dir}/{sanitized_stream_name}_{timestamp}.mp4");

        info!("FileSink recording to: {file_path}");

        // Fragmented MP4 (fMP4)
        // Each fragment written to disk is a complete `moof`+`mdat` pair
        // (same layout DASH/HLS/CMAF use).
        let _muxer = gst::ElementFactory::make("mp4mux")
            .property_from_str("fragment-mode", "dash-or-mss")
            .property("fragment-duration", 1000u32)
            .build()
            .context("Failed to create mp4mux")?;

        let _filesink = gst::ElementFactory::make("filesink")
            .property("location", &file_path)
            .build()
            .context("Failed to create filesink")?;

        // Create the pipeline
        let pipeline = gst::Pipeline::builder()
            .name(format!("pipeline-file-sink-{sink_id}"))
            .build();

        // Add Sink elements to the Sink's Pipeline
        let elements = [&_proxysrc, &_parse_element, &_muxer, &_filesink];
        if let Err(add_err) = pipeline.add_many(elements) {
            return Err(anyhow!(
                "Failed adding FileSink's elements to Sink Pipeline: {add_err:?}"
            ));
        }

        // Link proxysrc to parser
        if let Err(link_err) = _proxysrc.link(&_parse_element) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from FileSink Pipeline: {remove_err:?}");
            }
            return Err(anyhow!("Failed linking proxysrc to parser: {link_err:?}"));
        }

        // Link parser to mp4mux's video request pad
        let parser_src_pad = _parse_element
            .static_pad("src")
            .context("Failed to get parser src pad")?;
        let muxer_video_pad = _muxer
            .request_pad_simple("video_%u")
            .context("Failed to get mp4mux video pad")?;

        if let Err(link_err) = parser_src_pad.link(&muxer_video_pad) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from FileSink Pipeline: {remove_err:?}");
            }
            return Err(anyhow!("Failed linking parser to mp4mux: {link_err:?}"));
        }

        // Link mp4mux to filesink
        if let Err(link_err) = _muxer.link(&_filesink) {
            if let Err(remove_err) = pipeline.remove_many(elements) {
                warn!("Failed removing elements from FileSink Pipeline: {remove_err:?}");
            }
            return Err(anyhow!("Failed linking mp4mux to filesink: {link_err:?}"));
        }

        let pipeline_runner =
            PipelineRunner::try_new(&pipeline, &sink_id, true, video_and_stream_information)?;

        let recording_state = Arc::new(RwLock::new(RecordingState::Recording {
            start_time: std::time::Instant::now(),
            file_path: file_path.clone(),
        }));

        Ok(Self {
            sink_id,
            pipeline,
            proxysink,
            _proxysrc,
            _parse_element,
            _muxer,
            _filesink,
            tee_src_pad: None,
            pipeline_runner,
            recording_state,
            file_path,
            stream_name,
            encoding,
        })
    }

    /// Get the current recording state
    pub async fn recording_state(&self) -> RecordingState {
        self.recording_state.read().await.clone()
    }

    /// Get the recording duration in milliseconds (if recording)
    pub async fn recording_duration_ms(&self) -> u32 {
        let state = self.recording_state.read().await;
        match &*state {
            RecordingState::Recording { start_time, .. } => start_time.elapsed().as_millis() as u32,
            _ => 0,
        }
    }

    /// Get the file path of the recording
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// Get the stream name associated with this recording
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }

    /// Get the encoding type
    pub fn encoding(&self) -> &VideoEncodeType {
        &self.encoding
    }
}

/// Sanitize a string to be safe for use in filenames
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
