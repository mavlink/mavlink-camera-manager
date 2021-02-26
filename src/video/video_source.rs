use super::types::*;
use super::video_source_local::VideoSourceLocal;

pub trait VideoSource {
    fn name(&self) -> &String;
    fn source_string(&self) -> &String;
    fn formats(&self) -> Vec<Format>;
    fn set_control_by_name(&self, control_name: &str, value: i64) -> std::io::Result<()>;
    fn set_control_by_id(&self, control_id: u64, value: i64) -> std::io::Result<()>;
    fn control_value_by_name(&self, control_name: &str) -> std::io::Result<i64>;
    fn control_value_by_id(&self, control_id: u64) -> std::io::Result<i64>;
    fn cameras_available() -> Vec<VideoSourceType>;
    fn controls(&self) -> Vec<Control>;
}

pub fn cameras_available() -> Vec<VideoSourceType> {
    return VideoSourceLocal::cameras_available();
}

pub fn set_control(source_string: &str, control_id: u64, value: i64) -> std::io::Result<()> {
    let cameras = cameras_available();
    let camera = cameras
        .iter()
        .find(|source| source.inner().source_string() == source_string);

    if let Some(camera) = camera {
        return camera.inner().set_control_by_id(control_id, value);
    }

    let sources_available: Vec<String> = cameras
        .iter()
        .map(|source| source.inner().source_string().clone())
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
