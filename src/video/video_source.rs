use super::types::*;
use super::video_source_gst::VideoSourceGst;
use super::video_source_local::VideoSourceLocal;
use super::video_source_redirect::VideoSourceRedirect;
use log::*;
use simple_error::SimpleError;

pub trait VideoSource {
    fn name(&self) -> &String;
    fn source_string(&self) -> &str;
    fn formats(&self) -> Vec<Format>;
    fn set_control_by_name(&self, control_name: &str, value: i64) -> std::io::Result<()>;
    fn set_control_by_id(&self, control_id: u64, value: i64) -> std::io::Result<()>;
    fn control_value_by_name(&self, control_name: &str) -> std::io::Result<i64>;
    fn control_value_by_id(&self, control_id: u64) -> std::io::Result<i64>;
    fn controls(&self) -> Vec<Control>;
    fn is_valid(&self) -> bool;
    fn is_shareable(&self) -> bool;
}

pub trait VideoSourceAvailable {
    fn cameras_available() -> Vec<VideoSourceType>;
}

pub fn cameras_available() -> Vec<VideoSourceType> {
    return [
        &VideoSourceLocal::cameras_available()[..],
        &VideoSourceGst::cameras_available()[..],
        &VideoSourceRedirect::cameras_available()[..],
    ]
    .concat();
}

pub fn get_video_source(source_string: &str) -> Result<VideoSourceType, SimpleError> {
    match cameras_available()
        .iter()
        .find(|source| source.inner().source_string() == source_string)
    {
        Some(video_source) => Ok(video_source.clone()),
        None => Err(SimpleError::new(format!(
            "Failed to find video source for: {}",
            source_string
        ))),
    }
}

pub fn set_control(source_string: &str, control_id: u64, value: i64) -> std::io::Result<()> {
    let cameras = cameras_available();
    let camera = cameras
        .iter()
        .find(|source| source.inner().source_string() == source_string);

    if let Some(camera) = camera {
        debug!(
            "Set camera ({}) control ({}) value ({}).",
            source_string, control_id, value
        );
        return camera.inner().set_control_by_id(control_id, value);
    }

    let sources_available: Vec<String> = cameras
        .iter()
        .map(|source| source.inner().source_string().to_string())
        .collect();

    return Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "The source string '{}' does not exist, the available options are: {:?}.",
            source_string, sources_available
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_test() {
        println!("{:#?}", cameras_available());
    }
}
