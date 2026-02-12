use mcm_api::v1::{stream::VideoCaptureConfiguration, video::VideoSourceType};

use anyhow::Result;

pub trait VideoSourceLocalExt {
    fn try_identify_device(
        &mut self,
        capture_configuration: &VideoCaptureConfiguration,
        candidates: &[VideoSourceType],
    ) -> impl std::future::Future<Output = Result<Option<String>>> + Send;
}
