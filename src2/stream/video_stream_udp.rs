use super::gst::pipeline_runner::PipelineRunner;
use super::stream_backend::StreamBackend;

pub struct VideoStreamUdp {
    pipeline_runner: PipelineRunner,
}

impl Default for VideoStreamUdp {
    fn default() -> Self {
        Self {
            pipeline_runner: PipelineRunner::default(),
        }
    }
}

impl StreamBackend for VideoStreamUdp {
    fn start(&mut self) -> bool {
        self.pipeline_runner.run();
        return true;
    }

    fn stop(&mut self) -> bool {
        return true;
    }

    fn restart(&mut self) {
        unimplemented!();
        //self.pipeline_runner.restart();
    }
}
