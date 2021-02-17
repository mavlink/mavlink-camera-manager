use v4l::prelude::*;
use super::video_source_usb::{VideoSourceUsb, UsbBus};

#[derive(Debug)]
pub enum VideoSourceType {
    Usb(VideoSourceUsb),
}

#[derive(Debug)]
pub enum VideoEncodeType {
    UNKNOWN(String),
    H264,
    MJPG,
    YUYV,
}

impl VideoEncodeType {
    pub fn from_str(fourcc: &str) -> VideoEncodeType {
        return match fourcc {
            "H264" => VideoEncodeType::H264,
            "MJPG" => VideoEncodeType::MJPG,
            "YUYV" => VideoEncodeType::YUYV,
            _ => VideoEncodeType::UNKNOWN(fourcc.to_string()),
        };
    }
}

#[derive(Debug)]
pub struct FrameSize {
    pub encode: VideoEncodeType,
    pub height: u32,
    pub width: u32,
}

pub trait VideoSource {
    fn name(&self) -> &String;
    fn source_string(&self) -> &String;
    fn resolutions(&self) -> Vec<FrameSize>;
    fn configure_by_name(&self, config_name: &str, value: u32) -> bool;
    fn configure_by_id(&self, config_id: u32, value: u32) -> bool;
    fn cameras_available() -> Vec<VideoSourceType>;
    fn xml(&self) -> String;
}

pub fn cameras_available() -> Vec<VideoSourceType> {
    return VideoSourceUsb::cameras_available();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_test() {
        println!("{:#?}", cameras_available());
    }
}
