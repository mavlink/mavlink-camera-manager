pub trait StreamBackend {
    fn start(&mut self) -> bool;
    fn stop(&mut self) -> bool;
    fn restart(&mut self);
    fn set_pipeline_description(&mut self, description: &'static str);
}
