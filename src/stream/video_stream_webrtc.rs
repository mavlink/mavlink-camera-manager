use super::{
    gst::pipeline_runner::{Pipeline, PipelineRunner},
    signalling_server::DEFAULT_SIGNALLING_ENDPOINT,
    stream_backend::StreamBackend,
    turn_server::{TurnServer, DEFAULT_STUN_ENDPOINT, DEFAULT_TURN_ENDPOINT},
};

#[derive(Debug)]
#[allow(dead_code)]
pub struct VideoStreamWebRTC {
    pipeline_runner: PipelineRunner,
}

impl Default for VideoStreamWebRTC {
    fn default() -> Self {
        Self {
            pipeline_runner: PipelineRunner::new(Pipeline {
                description: format!(
                    concat!(
                        "videotestsrc pattern={pattern}",
                        " ! video/x-raw,{video_format}",
                        " ! videoconvert",
                        " ! webrtcsink stun-server={stun_server} turn-server={turn_server} signaller::address={signaller_server}",
                        " video-caps=video/x-h264,{video_format}",
                    ),
                    pattern = "blink",
                    video_format = "width=640,height=480,framerate=30/1",
                    stun_server = url::Url::parse(DEFAULT_STUN_ENDPOINT).unwrap(),
                    turn_server = url::Url::parse(DEFAULT_TURN_ENDPOINT).unwrap(),
                    signaller_server = url::Url::parse(DEFAULT_SIGNALLING_ENDPOINT).unwrap(),
                )
                .to_string(),
            }),
        }
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

    fn set_pipeline_description(&mut self, description: &str) {
        self.pipeline_runner.set_pipeline_description(description);
    }

    fn pipeline(&self) -> String {
        self.pipeline_runner.pipeline()
    }

    fn allow_same_endpoints(&self) -> bool {
        true
    }
}
