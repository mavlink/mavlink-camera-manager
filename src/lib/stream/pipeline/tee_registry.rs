use gst::prelude::*;

use crate::{stream::sink::Sink, video::types::VideoEncodeType};

#[derive(Debug)]
pub enum TeeMedia {
    Raw(VideoEncodeType),
    Compressed(VideoEncodeType),
    Rtp,
}

#[derive(Debug)]
pub struct PipelineTee {
    pub name: String,
    pub element: gst::Element,
    pub media: TeeMedia,
}

#[derive(Debug)]
pub struct TeeRegistry {
    tees: Vec<PipelineTee>,
}

impl TeeRegistry {
    pub fn new() -> Self {
        Self { tees: Vec::new() }
    }

    pub fn register(&mut self, name: String, element: gst::Element, media: TeeMedia) {
        self.tees.push(PipelineTee {
            name,
            element,
            media,
        });
    }

    pub fn tee_for_sink(&self, sink: &Sink) -> Option<&PipelineTee> {
        match sink {
            Sink::Image(_) => self
                .tees
                .iter()
                .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Raw(_)))
                .or_else(|| {
                    self.tees
                        .iter()
                        .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Compressed(_)))
                }),
            Sink::Zenoh(_) | Sink::Rtsp(_) => self
                .tees
                .iter()
                .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Compressed(_)))
                .or_else(|| {
                    self.tees
                        .iter()
                        .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Raw(_)))
                }),
            Sink::Udp(_) | Sink::WebRTC(_) => self
                .tees
                .iter()
                .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Rtp)),
        }
    }

    pub fn raw_tee(&self) -> Option<&gst::Element> {
        self.tees
            .iter()
            .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Raw(_)))
            .map(|pipeline_tee| &pipeline_tee.element)
    }

    pub fn compressed_tee(&self) -> Option<&gst::Element> {
        self.tees
            .iter()
            .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Compressed(_)))
            .map(|pipeline_tee| &pipeline_tee.element)
    }

    pub fn rtp_tee(&self) -> Option<&gst::Element> {
        self.tees
            .iter()
            .find(|pipeline_tee| matches!(pipeline_tee.media, TeeMedia::Rtp))
            .map(|pipeline_tee| &pipeline_tee.element)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_and_rtp_tee_lookup() {
        gst::init().unwrap();

        let mut registry = TeeRegistry::new();
        let compressed = gst::ElementFactory::make("tee")
            .name("VideoTee-test")
            .build()
            .unwrap();
        let rtp = gst::ElementFactory::make("tee")
            .name("RTPTee-test")
            .build()
            .unwrap();
        registry.register(
            "VideoTee-test".to_string(),
            compressed,
            TeeMedia::Compressed(VideoEncodeType::H264),
        );
        registry.register("RTPTee-test".to_string(), rtp, TeeMedia::Rtp);

        assert_eq!(registry.compressed_tee().unwrap().name(), "VideoTee-test");
        assert_eq!(registry.rtp_tee().unwrap().name(), "RTPTee-test");
    }

    #[test]
    fn raw_tee_lookup_and_image_like_fallback() {
        gst::init().unwrap();

        let mut registry = TeeRegistry::new();
        let raw = gst::ElementFactory::make("tee")
            .name("VideoTee-raw")
            .build()
            .unwrap();
        let rtp = gst::ElementFactory::make("tee")
            .name("RTPTee-test")
            .build()
            .unwrap();
        registry.register(
            "VideoTee-raw".to_string(),
            raw,
            TeeMedia::Raw(VideoEncodeType::Nv12),
        );
        registry.register("RTPTee-test".to_string(), rtp, TeeMedia::Rtp);

        assert_eq!(registry.raw_tee().unwrap().name(), "VideoTee-raw");
        assert!(registry.compressed_tee().is_none());
        assert_eq!(
            registry
                .compressed_tee()
                .or_else(|| registry.raw_tee())
                .unwrap()
                .name(),
            "VideoTee-raw"
        );
    }

    #[test]
    fn image_prefers_raw_and_rtsp_prefers_compressed_when_both_registered() {
        gst::init().unwrap();

        let mut registry = TeeRegistry::new();
        let raw = gst::ElementFactory::make("tee")
            .name("VideoTee-raw")
            .build()
            .unwrap();
        let compressed = gst::ElementFactory::make("tee")
            .name("VideoTee-compressed")
            .build()
            .unwrap();
        registry.register(
            "VideoTee-raw".to_string(),
            raw,
            TeeMedia::Raw(VideoEncodeType::Nv12),
        );
        registry.register(
            "VideoTee-compressed".to_string(),
            compressed,
            TeeMedia::Compressed(VideoEncodeType::H264),
        );

        assert_eq!(
            registry
                .raw_tee()
                .or_else(|| registry.compressed_tee())
                .unwrap()
                .name(),
            "VideoTee-raw"
        );
        assert_eq!(
            registry
                .compressed_tee()
                .or_else(|| registry.raw_tee())
                .unwrap()
                .name(),
            "VideoTee-compressed"
        );
    }
}
