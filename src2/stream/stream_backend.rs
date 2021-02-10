pub enum StreamType {
    RTSP(),
    UDP(),
}

pub trait StreamBackend {
    fn start(&mut self) -> bool;
    fn stop(&mut self) -> bool;
    fn restart(&mut self);
}
