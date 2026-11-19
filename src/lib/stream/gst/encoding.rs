use gst::prelude::*;

use crate::video::types::VideoEncodeType;

pub trait CompressedEncoding: Sync {
    fn encode_type(&self) -> VideoEncodeType;
    fn encode_key(&self) -> &'static str;
    fn caps_mime(&self) -> &'static str;
    fn optional_parser_factory(&self) -> Option<&'static str>;
    fn configure_parser_element(&self, parser: &gst::Element);
    fn pay_factory_name(&self) -> &'static str;
    fn configure_pay_element(&self, pay: &gst::Element);
    fn preferred_encoder_factory(&self) -> &'static str;
    fn compressed_caps(&self, width: u32, height: u32) -> gst::Caps;
}

pub struct H264;
pub struct H265;
pub struct Mjpg;

impl CompressedEncoding for H264 {
    fn encode_type(&self) -> VideoEncodeType {
        VideoEncodeType::H264
    }

    fn encode_key(&self) -> &'static str {
        "H264"
    }

    fn caps_mime(&self) -> &'static str {
        "video/x-h264"
    }

    fn optional_parser_factory(&self) -> Option<&'static str> {
        Some("h264parse")
    }

    fn configure_parser_element(&self, parser: &gst::Element) {
        parser.set_property("config-interval", -1i32);
    }

    fn pay_factory_name(&self) -> &'static str {
        "rtph264pay"
    }

    fn configure_pay_element(&self, pay: &gst::Element) {
        pay.set_property_from_str("aggregate-mode", "zero-latency");
        pay.set_property("config-interval", -1i32);
        pay.set_property("pt", 96u32);
    }

    fn preferred_encoder_factory(&self) -> &'static str {
        "x264enc"
    }

    fn compressed_caps(&self, width: u32, height: u32) -> gst::Caps {
        gst::Caps::builder("video/x-h264")
            .field("stream-format", "avc")
            .field("alignment", "au")
            .field("width", width as i32)
            .field("height", height as i32)
            .build()
    }
}

impl CompressedEncoding for H265 {
    fn encode_type(&self) -> VideoEncodeType {
        VideoEncodeType::H265
    }

    fn encode_key(&self) -> &'static str {
        "H265"
    }

    fn caps_mime(&self) -> &'static str {
        "video/x-h265"
    }

    fn optional_parser_factory(&self) -> Option<&'static str> {
        Some("h265parse")
    }

    fn configure_parser_element(&self, parser: &gst::Element) {
        parser.set_property("config-interval", -1i32);
    }

    fn pay_factory_name(&self) -> &'static str {
        "rtph265pay"
    }

    fn configure_pay_element(&self, pay: &gst::Element) {
        pay.set_property_from_str("aggregate-mode", "zero-latency");
        pay.set_property("config-interval", -1i32);
        pay.set_property("pt", 96u32);
    }

    fn preferred_encoder_factory(&self) -> &'static str {
        "x265enc"
    }

    fn compressed_caps(&self, width: u32, height: u32) -> gst::Caps {
        gst::Caps::builder("video/x-h265")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .field("width", width as i32)
            .field("height", height as i32)
            .build()
    }
}

impl CompressedEncoding for Mjpg {
    fn encode_type(&self) -> VideoEncodeType {
        VideoEncodeType::Mjpg
    }

    fn encode_key(&self) -> &'static str {
        "MJPG"
    }

    fn caps_mime(&self) -> &'static str {
        "image/jpeg"
    }

    fn optional_parser_factory(&self) -> Option<&'static str> {
        // jpegparse spoils caps negotiation on the passthrough path; skip it here too.
        None
    }

    fn configure_parser_element(&self, _parser: &gst::Element) {}

    fn pay_factory_name(&self) -> &'static str {
        "rtpjpegpay"
    }

    fn configure_pay_element(&self, pay: &gst::Element) {
        pay.set_property("pt", 96u32);
    }

    fn preferred_encoder_factory(&self) -> &'static str {
        "jpegenc"
    }

    fn compressed_caps(&self, width: u32, height: u32) -> gst::Caps {
        gst::Caps::builder("image/jpeg")
            .field("width", width as i32)
            .field("height", height as i32)
            .build()
    }
}

pub fn encodings() -> &'static [&'static dyn CompressedEncoding] {
    &[&H264, &H265, &Mjpg]
}

pub fn encoding(encode: &VideoEncodeType) -> Option<&'static dyn CompressedEncoding> {
    encodings()
        .iter()
        .copied()
        .find(|item| item.encode_type() == *encode)
}

pub fn preferred_encoder_factory_name(encode: &VideoEncodeType) -> &'static str {
    encoding(encode)
        .map(|item| item.preferred_encoder_factory())
        .unwrap_or("x264enc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_has_parser_mjpg_does_not() {
        assert!(H264.optional_parser_factory().is_some());
        assert!(H265.optional_parser_factory().is_some());
        assert!(Mjpg.optional_parser_factory().is_none());
        assert_eq!(encodings().len(), 3);
        assert_eq!(
            encoding(&VideoEncodeType::H264).unwrap().encode_key(),
            "H264"
        );
        assert_eq!(
            encoding(&VideoEncodeType::H265).unwrap().encode_key(),
            "H265"
        );
        assert_eq!(
            encoding(&VideoEncodeType::Mjpg).unwrap().encode_key(),
            "MJPG"
        );
    }

    #[test]
    fn h265_compressed_caps() {
        let _ = gst::init();
        let caps = H265.compressed_caps(1920, 1080);
        let structure = caps.structure(0).unwrap();
        assert_eq!(structure.name(), "video/x-h265");
        assert_eq!(
            structure.get::<&str>("stream-format").unwrap(),
            "byte-stream"
        );
        assert_eq!(structure.get::<&str>("alignment").unwrap(), "au");
        assert_eq!(structure.get::<i32>("width").unwrap(), 1920);
        assert_eq!(structure.get::<i32>("height").unwrap(), 1080);
    }
}
