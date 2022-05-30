use simple_error::SimpleError;

use crate::video_stream::types::VideoAndStreamInformation;

use super::{
    gst::pipeline_builder::Pipeline, gst::pipeline_runner::PipelineRunner,
    stream_backend::StreamBackend, webrtc::turn_server::TurnServer,
};

#[derive(Debug)]
#[allow(dead_code)]
pub struct VideoStreamWebRTC {
    pipeline_runner: PipelineRunner,
}

impl VideoStreamWebRTC {
    pub fn new(
        video_and_stream_information: &VideoAndStreamInformation,
    ) -> Result<Self, SimpleError> {
        Ok(Self {
            pipeline_runner: PipelineRunner::new(Pipeline::new(video_and_stream_information)?),
        })
    }
}

impl Drop for VideoStreamWebRTC {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StreamBackend for VideoStreamWebRTC {
    fn start(&mut self) -> bool {
        TurnServer::notify_start();
        self.pipeline_runner.start()
    }

    fn stop(&mut self) -> bool {
        TurnServer::notify_stop();
        self.pipeline_runner.stop()
    }

    fn restart(&mut self) {
        self.pipeline_runner.restart()
    }

    fn is_running(&self) -> bool {
        self.pipeline_runner.is_running()
    }

    fn pipeline(&self) -> String {
        self.pipeline_runner.pipeline()
    }

    fn allow_same_endpoints(&self) -> bool {
        true
    }
}
