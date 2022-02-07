use super::stream_backend::StreamBackend;

#[derive(Debug)]
pub struct VideoStreamRedirect {
    pub scheme: String,
}

impl Default for VideoStreamRedirect {
    fn default() -> Self {
        Self {
            scheme: "udp".into(),
        }
    }
}

impl Drop for VideoStreamRedirect {
    fn drop(&mut self) {}
}

impl StreamBackend for VideoStreamRedirect {
    fn start(&mut self) -> bool {
        true
    }

    fn stop(&mut self) -> bool {
        true
    }

    fn restart(&mut self) {
        unimplemented!();
    }

    fn is_running(&self) -> bool {
        true
    }

    fn set_pipeline_description(&mut self, _description: &str) {}

    fn pipeline(&self) -> String {
        "".into()
    }
}
