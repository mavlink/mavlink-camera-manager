use regex::Regex;
use v4l::prelude::*;
use v4l::video::Capture;
use v4l::FrameSize;

#[derive(Debug)]
pub enum VideoSourceType {
    Usb(VideoSourceUsb),
}

#[derive(Debug)]
pub struct UsbBus {
    pub domain: u8,
    pub bus: u8,
    pub device: u8,
    pub first_function: u8,
    pub last_function: u8,
}

#[derive(Debug)]
pub struct VideoSourceUsb {
    name: String,
    device_path: String,
    pub usb_bus: UsbBus,
}

impl UsbBus {
    // https://wiki.xenproject.org/wiki/Bus:Device.Function_(BDF)_Notation
    // description should follow: <domain>:<bus>:<device>.<first_function>-<last_function>
    pub fn from_str(description: &str) -> std::io::Result<Self> {
        let regex = Regex::new(
            r"(?P<domain>\d+):(?P<bus>\d+):(?P<device>\d+).(?P<first_function>\d+)-(?P<last_function>\d+)",
        )
        .unwrap();
        if !regex.is_match(description) {
            panic!("Description is not valid: {:#?}", description);
        }

        let capture = regex.captures(description).unwrap();
        let domain = capture.name("domain").unwrap().as_str().parse().unwrap();
        let bus = capture.name("bus").unwrap().as_str().parse().unwrap();
        let device = capture.name("device").unwrap().as_str().parse().unwrap();
        let first_function = capture
            .name("first_function")
            .unwrap()
            .as_str()
            .parse()
            .unwrap();
        let last_function = capture
            .name("last_function")
            .unwrap()
            .as_str()
            .parse()
            .unwrap();

        return Ok(Self {
            domain,
            bus,
            device,
            first_function,
            last_function,
        });
    }
}

pub trait VideoSource {
    fn name(&self) -> &String;
    fn source_string(&self) -> &String;
    fn resolutions(&self) -> Vec<FrameSize>;
    fn configure_by_name(&self, config_name: &str, value: u32) -> bool;
    fn configure_by_id(&self, config_id: u32, value: u32) -> bool;
}

pub fn local_cameras_available() -> Vec<VideoSourceType> {
    // Extract path of video devices
    let cameras_path = std::fs::read_dir("/dev/")
        .unwrap()
        .filter(|f| {
            f.as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .starts_with("video")
        })
        .map(|f| String::from(f.unwrap().path().clone().to_str().unwrap()))
        .collect::<Vec<_>>();

    let mut cameras: Vec<VideoSourceType> = vec![];
    for camera_path in &cameras_path {
        let camera = Device::with_path(camera_path).unwrap();
        let caps = camera.query_caps().unwrap();
        cameras.push(VideoSourceType::Usb(VideoSourceUsb {
            name: caps.card,
            device_path: camera_path.clone(),
            usb_bus: UsbBus::from_str(&caps.bus).unwrap(),
        }));
    }

    return cameras;
}

impl VideoSource for VideoSourceUsb {
    fn name(&self) -> &String {
        return &self.name;
    }

    fn source_string(&self) -> &String {
        return &self.device_path;
    }

    fn resolutions(&self) -> Vec<FrameSize> {
        let device = Device::with_path(&self.device_path).unwrap();
        let format = device.format().unwrap();
        let frame_sizes = device.enum_framesizes(format.fourcc).unwrap();
        return frame_sizes;
    }

    fn configure_by_name(&self, config_name: &str, value: u32) -> bool {
        unimplemented!();
    }
    fn configure_by_id(&self, config_id: u32, value: u32) -> bool {
        unimplemented!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_test() {
        println!("{:#?}", local_cameras_available());
    }
}
