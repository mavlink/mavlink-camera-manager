use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use tracing::*;

use crate::{
    stream::types::CaptureConfiguration,
    video::{
        types::{VideoEncodeType, VideoSourceType},
        video_source_gst::{VideoSourceGstType, file_demuxer},
    },
    video_stream::types::VideoAndStreamInformation,
};

use super::{
    PIPELINE_FILTER_NAME, PIPELINE_RTP_TEE_NAME, PIPELINE_VIDEO_TEE_NAME,
    PipelineGstreamerInterface, PipelineState,
};

#[derive(Debug)]
pub struct FilePipeline {
    pub state: PipelineState,
}

impl FilePipeline {
    #[instrument(level = "debug", skip_all)]
    pub fn try_new(
        pipeline_id: &Arc<uuid::Uuid>,
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<gst::Pipeline> {
        let configuration = match &video_and_stream_information
            .stream_information
            .configuration
        {
            CaptureConfiguration::Video(configuration) => configuration,
            unsupported => {
                return Err(anyhow!("{unsupported:?} is not supported as File Pipeline"));
            }
        };

        let path = match &video_and_stream_information.video_source {
            VideoSourceType::Gst(source) => match &source.source {
                VideoSourceGstType::File(path) => path,
                unsupported => {
                    return Err(anyhow!(
                        "VideoSourceGstType {unsupported:?} is not supported as File Pipeline"
                    ));
                }
            },
            unsupported => {
                return Err(anyhow!(
                    "SourceType {unsupported:?} is not supported as File Pipeline"
                ));
            }
        };

        let (parser, caps, payloader) = match &configuration.encode {
            VideoEncodeType::H264 => (
                "h264parse",
                "video/x-h264,stream-format=avc,alignment=au",
                "rtph264pay",
            ),
            VideoEncodeType::H265 => (
                "h265parse",
                "video/x-h265,stream-format=byte-stream,alignment=au",
                "rtph265pay",
            ),
            unsupported => {
                return Err(anyhow!(
                    "Encode {unsupported:?} is not supported for File Pipeline"
                ));
            }
        };

        let source_name = format!("FileSource-{pipeline_id}");
        let demux_name = format!("FileDemux-{pipeline_id}");
        let sync_name = format!("FileSync-{pipeline_id}");
        let filter_name = format!("{PIPELINE_FILTER_NAME}-{pipeline_id}");
        let video_tee_name = format!("{PIPELINE_VIDEO_TEE_NAME}-{pipeline_id}");
        let rtp_tee_name = format!("{PIPELINE_RTP_TEE_NAME}-{pipeline_id}");

        let description = format!(
            concat!(
                "filesrc name={source_name}",
                " ! {demuxer} name={demux_name}",
                // Play at real time, before the parser so the loop probe can't race to the end of the file
                " ! identity name={sync_name} sync=true",
                " ! {parser} config-interval=-1",
                // Keep the timestamps growing over the loops
                " ! identity single-segment=true",
                " ! capsfilter name={filter_name} caps={caps},width={width},height={height},framerate={interval_denominator}/{interval_numerator}",
                " ! tee name={video_tee_name} allow-not-linked=true",
                " ! {payloader} aggregate-mode=zero-latency config-interval=-1 pt=96",
                " ! tee name={rtp_tee_name} allow-not-linked=true"
            ),
            source_name = source_name,
            demuxer = file_demuxer(path)?,
            demux_name = demux_name,
            sync_name = sync_name,
            parser = parser,
            filter_name = filter_name,
            caps = caps,
            width = configuration.width,
            height = configuration.height,
            interval_denominator = configuration.frame_interval.denominator,
            interval_numerator = configuration.frame_interval.numerator,
            video_tee_name = video_tee_name,
            payloader = payloader,
            rtp_tee_name = rtp_tee_name,
        );

        debug!("pipeline_description: {description:#?}");

        let pipeline = gst::parse::launch(&description)?;

        let pipeline = pipeline
            .downcast::<gst::Pipeline>()
            .expect("Couldn't downcast pipeline");

        pipeline
            .by_name(&source_name)
            .context("No filesrc in File Pipeline")?
            .set_property("location", path);

        loop_file(
            &pipeline
                .by_name(&demux_name)
                .context("No demuxer in File Pipeline")?,
            &pipeline
                .by_name(&sync_name)
                .context("No identity in File Pipeline")?,
        )?;

        pipeline.set_property("name", format!("pipeline-file-{pipeline_id}"));

        Ok(pipeline)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LoopState {
    Waiting,
    Seeking,
    Looping,
}

struct Loop {
    state: LoopState,
    segment: gst::FormattedSegment<gst::ClockTime>,
    /// Running time where the last buffer ends, the next loop starts there
    end: Option<gst::ClockTime>,
    last_pts: Option<gst::ClockTime>,
}

/// Plays the file forever using non-flushing segment seeks on the demuxer.
/// The pipeline never sees an EOS and the running time keeps growing.
/// The probe goes in the element right after the demuxer.
pub fn loop_file(demux: &gst::Element, element: &gst::Element) -> Result<()> {
    let state = Mutex::new(Loop {
        state: LoopState::Waiting,
        segment: gst::FormattedSegment::new(),
        end: None,
        last_pts: None,
    });
    let demux_weak = demux.downgrade();

    element
        .static_pad("sink")
        .context("No sink pad after the demuxer")?
        .add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
            move |_pad, info| {
                let mut state = state.lock().unwrap();

                if let Some(buffer) = info.buffer() {
                    // Drop what the demuxer pushes before the first seek takes place
                    if state.state == LoopState::Seeking {
                        return gst::PadProbeReturn::Drop;
                    }
                    if let Some(pts) = buffer.pts() {
                        let duration = buffer
                            .duration()
                            .or_else(|| state.last_pts.and_then(|last| pts.checked_sub(last)))
                            .unwrap_or_default();
                        if let Some(running_time) = state.segment.to_running_time(pts) {
                            state.end = Some(running_time + duration);
                        }
                        state.last_pts = Some(pts);
                    }
                    return gst::PadProbeReturn::Ok;
                }

                let Some(event) = info.event() else {
                    return gst::PadProbeReturn::Ok;
                };

                match (event.view(), state.state) {
                    (gst::EventView::Segment(_), LoopState::Waiting) => {
                        state.state = LoopState::Seeking;
                        seek_to_start(&demux_weak);
                    }
                    (gst::EventView::Segment(segment), loop_state) => {
                        let Some(mut segment) =
                            segment.segment().downcast_ref::<gst::ClockTime>().cloned()
                        else {
                            return gst::PadProbeReturn::Ok;
                        };
                        // Older demuxers (GStreamer < 1.26) start the next loop at the last buffer
                        // timestamp instead of its end, repeating one timestamp per loop
                        if let (LoopState::Looping, Some(end)) = (loop_state, state.end) {
                            segment.set_base(end);
                            let event = gst::event::Segment::builder(&segment)
                                .seqnum(event.seqnum())
                                .build();
                            info.data = Some(gst::PadProbeData::Event(event));
                        }
                        state.segment = segment;
                        state.last_pts = None;
                        state.state = LoopState::Looping;
                    }
                    (gst::EventView::SegmentDone(_), _) => {
                        seek_to_start(&demux_weak);
                        return gst::PadProbeReturn::Drop;
                    }
                    _ => (),
                }

                gst::PadProbeReturn::Ok
            },
        )
        .context("Failed to add loop probe")?;

    Ok(())
}

fn seek_to_start(demux_weak: &gst::glib::WeakRef<gst::Element>) {
    let Some(demux) = demux_weak.upgrade() else {
        return;
    };

    // qtdemux blocks if we seek from its streaming thread.
    // A flushing seek is also not an option, it can deadlock with the pipeline state changes.
    demux.call_async(|demux| {
        if let Err(error) = demux.seek(
            1.0,
            gst::SeekFlags::SEGMENT,
            gst::SeekType::Set,
            gst::ClockTime::ZERO,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        ) {
            warn!("Failed to loop file: {error:?}");
        }
    });
}

impl PipelineGstreamerInterface for FilePipeline {
    #[instrument(level = "trace")]
    fn is_running(&self) -> bool {
        self.state.pipeline_runner.is_running()
    }
}
