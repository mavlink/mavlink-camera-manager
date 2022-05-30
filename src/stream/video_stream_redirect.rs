use simple_error::SimpleError;

use super::stream_backend::StreamBackend;

#[derive(Debug)]
pub struct VideoStreamRedirect {
    pub scheme: String,
}

impl VideoStreamRedirect {
    pub fn new(scheme: String) -> Result<Self, SimpleError> {
        Ok(Self { scheme })
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

    fn pipeline(&self) -> String {
        "".into()
    }

    fn allow_same_endpoints(&self) -> bool {
        false
    }
}
