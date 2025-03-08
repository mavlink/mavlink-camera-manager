use tracing::*;

use crate::controls::types::{Control, ControlType};

use super::{
    types::*, video_source_gst::VideoSourceGst, video_source_local::VideoSourceLocal,
    video_source_onvif::VideoSourceOnvif, video_source_redirect::VideoSourceRedirect,
};

pub trait VideoSource {
    fn name(&self) -> &String;
    fn source_string(&self) -> &str;
    fn set_control_by_name(&self, control_name: &str, value: i64) -> std::io::Result<()>;
    fn set_control_by_id(&self, control_id: u64, value: i64) -> std::io::Result<()>;
    fn control_value_by_name(&self, control_name: &str) -> std::io::Result<i64>;
    fn control_value_by_id(&self, control_id: u64) -> std::io::Result<i64>;
    fn controls(&self) -> Vec<Control>;
    fn is_valid(&self) -> bool;
    fn is_shareable(&self) -> bool;
}

pub(crate) trait VideoSourceFormats {
    async fn formats(&self) -> Vec<Format>;
}

pub(crate) trait VideoSourceAvailable {
    async fn cameras_available() -> Vec<VideoSourceType>;
}

pub async fn cameras_available() -> Vec<VideoSourceType> {
    [
        &VideoSourceLocal::cameras_available().await[..],
        &VideoSourceGst::cameras_available().await[..],
        &VideoSourceOnvif::cameras_available().await[..],
        &VideoSourceRedirect::cameras_available().await[..],
    ]
    .concat()
}

pub async fn get_video_source(source_string: &str) -> Result<VideoSourceType, std::io::Error> {
    let cameras = cameras_available().await;

    if let Some(camera) = cameras
        .iter()
        .find(|source| source.inner().source_string() == source_string)
    {
        return Ok(camera.clone());
    }

    let sources_available: Vec<String> = cameras
        .iter()
        .map(|source| source.inner().source_string().to_string())
        .collect();

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "The source string '{source_string}' does not exist, the available options are: {sources_available:?}."
        ),
    ))
}

pub async fn set_control(source_string: &str, control_id: u64, value: i64) -> std::io::Result<()> {
    let camera = get_video_source(source_string).await?;
    debug!("Set camera ({source_string}) control ({control_id}) value ({value}).");
    camera.inner().set_control_by_id(control_id, value)
}

pub async fn reset_controls(source_string: &str) -> Result<(), Vec<std::io::Error>> {
    let camera = match get_video_source(source_string).await {
        Ok(camera) => camera,
        Err(error) => return Err(vec![error]),
    };

    debug!("Resetting all controls of camera ({source_string}).",);

    let mut errors: Vec<std::io::Error> = Default::default();
    for control in camera.inner().controls() {
        if control.state.is_inactive {
            continue;
        }

        let default_value = match &control.configuration {
            ControlType::Bool(bool) => bool.default,
            ControlType::Slider(slider) => slider.default,
            ControlType::Menu(menu) => menu.default,
        };

        if let Err(error) = camera.inner().set_control_by_id(control.id, default_value) {
            let error_message = format!(
                "Error when trying to reset control '{}' (id {}). Error: {error}.",
                control.name, control.id,
            );
            errors.push(std::io::Error::new(error.kind(), error_message));
        }
    }
    if errors.is_empty() {
        return Ok(());
    }

    error!("{errors:#?}");
    Err(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn simple_test() {
        println!("{:#?}", cameras_available().await);
    }
}
