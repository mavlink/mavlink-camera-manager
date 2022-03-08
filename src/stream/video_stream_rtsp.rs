use super::gst::pipeline_runner::Pipeline;
use super::stream_backend::StreamBackend;

use super::rtsp_server::RTSPServer;

#[derive(Debug)]
pub struct VideoStreamRtsp {
    pipeline: Pipeline,
    endpoint_path: String,
}

impl Default for VideoStreamRtsp {
    fn default() -> Self {
        Self {
            pipeline: Pipeline {
                description: "videotestsrc pattern=ball ! video/x-raw,width=640,height=480,framerate=30/1 ! videoconvert ! x264enc bitrate=5000 ! video/x-h264, profile=baseline ! h264parse ! queue ! rtph264pay name=pay0".into()
            },
            endpoint_path: "/test".into(),
        }
    }
}

impl VideoStreamRtsp {
    pub fn set_endpoint_path(&mut self, path: &str) {
        self.endpoint_path = path.into();
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

    fn set_pipeline_description(&mut self, description: &str) {
        RTSPServer::add_pipeline(description, &self.endpoint_path);
        self.pipeline.description = description.to_string();
    }

    fn pipeline(&self) -> String {
        self.pipeline.description.clone()
    }

    fn allow_same_endpoints(&self) -> bool {
        false
    }
}
