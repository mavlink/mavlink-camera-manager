use super::{
    gst::pipeline_builder::Pipeline, gst::pipeline_runner::PipelineRunner,
    stream_backend::StreamBackend,
};

#[derive(Debug)]
#[allow(dead_code)]
pub struct VideoStreamUdp {
    pipeline_runner: PipelineRunner,
}

impl VideoStreamUdp {
    pub fn new(
        video_and_stream_information: &crate::video_stream::types::VideoAndStreamInformation,
    ) -> Result<Self, simple_error::SimpleError> {
        Ok(VideoStreamUdp {
            pipeline_runner: PipelineRunner::new(Pipeline::new(video_and_stream_information)?),
        })
    }
}

impl Drop for VideoStreamUdp {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StreamBackend for VideoStreamUdp {
    fn start(&mut self) -> bool {
        self.pipeline_runner.start()
    }

    fn stop(&mut self) -> bool {
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
        false
    }
}
