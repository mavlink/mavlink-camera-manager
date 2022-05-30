use super::gst::pipeline_builder::Pipeline;
use super::stream_backend::StreamBackend;

use super::rtsp_server::RTSPServer;

#[derive(Debug)]
pub struct VideoStreamRtsp {
    pipeline: Pipeline,
    endpoint_path: String,
}

impl VideoStreamRtsp {
    pub fn new(
        video_and_stream_information: &crate::video_stream::types::VideoAndStreamInformation,
        endpoint_path: String,
    ) -> Result<Self, simple_error::SimpleError> {
        let pipeline = Pipeline::new(video_and_stream_information)?;
        RTSPServer::add_pipeline(&pipeline.description, &endpoint_path)?;
        Ok(VideoStreamRtsp {
            pipeline,
            endpoint_path,
        })
    }
}

impl Drop for VideoStreamRtsp {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StreamBackend for VideoStreamRtsp {
    fn start(&mut self) -> bool {
        RTSPServer::start_pipeline(&self.endpoint_path);
        true
    }

    fn stop(&mut self) -> bool {
        RTSPServer::stop_pipeline(&self.endpoint_path);
        true
    }

    fn restart(&mut self) {
        unimplemented!();
    }

    fn is_running(&self) -> bool {
        RTSPServer::is_running()
    }

    fn pipeline(&self) -> String {
        self.pipeline.description.clone()
    }

    fn allow_same_endpoints(&self) -> bool {
        false
    }
}
