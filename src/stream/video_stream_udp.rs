use super::{
    gst::pipeline_runner::{Pipeline, PipelineRunner},
    stream_backend::StreamBackend,
};

#[derive(Debug)]
#[allow(dead_code)]
pub struct VideoStreamUdp {
    pipeline_runner: PipelineRunner,
}

impl Default for VideoStreamUdp {
    fn default() -> Self {
        Self {
            pipeline_runner: PipelineRunner::new(Pipeline {
                description: concat!(
                    "videotestsrc pattern=blink",
                    " ! video/x-raw,width=640,height=480",
                    " ! videoconvert",
                    " ! x264enc bitrate=5000",
                    " ! video/x-h264, profile=baseline",
                    " ! rtph264pay",
                    " ! udpsink host=0.0.0.0 port=5600",
                )
                .to_string(),
            }),
        }
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

    fn set_pipeline_description(&mut self, description: &str) {
        self.pipeline_runner.set_pipeline_description(description);
    }

    fn pipeline(&self) -> String {
        self.pipeline_runner.pipeline()
    }

    fn allow_same_endpoints(&self) -> bool {
        false
    }
}
