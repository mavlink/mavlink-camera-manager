use v4l::prelude::*;
use v4l::FrameSize;

use super::video_source_usb::{VideoSourceUsb, UsbBus};

#[derive(Debug)]
pub enum VideoSourceType {
    Usb(VideoSourceUsb),
}

pub trait VideoSource {
    fn name(&self) -> &String;
    fn source_string(&self) -> &String;
    fn resolutions(&self) -> Vec<FrameSize>;
    fn configure_by_name(&self, config_name: &str, value: u32) -> bool;
    fn configure_by_id(&self, config_id: u32, value: u32) -> bool;
    fn xml(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_test() {
        println!("{:#?}", VideoSourceUsb::cameras_available());
    }
}
