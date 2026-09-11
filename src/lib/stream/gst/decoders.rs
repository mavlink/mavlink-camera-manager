use std::collections::BTreeMap;

use paperclip::actix::Apiv2Schema;
use serde::Serialize;
use tracing::error;

use gst::prelude::*;

use super::docs::gst_element_docs_url;
use super::encoders::{
    EncoderFactoryDetails, EncoderInfo, EncoderPluginDetails, GStreamerBuildInfo,
};
use crate::video::types::VideoEncodeType;

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct Decoders {
    pub gstreamer: GStreamerBuildInfo,
    pub decodings: BTreeMap<String, Vec<EncoderInfo>>,
}

pub fn decoders() -> Decoders {
    if let Err(error) = gst::init() {
        error!("{error}");
        return Decoders {
            gstreamer: gstreamer_build_info(),
            decodings: BTreeMap::new(),
        };
    }

    let mut decodings = BTreeMap::new();
    for source_encode in [
        VideoEncodeType::Mjpg,
        VideoEncodeType::H264,
        VideoEncodeType::H265,
    ] {
        decodings.insert(
            source_encode_key(&source_encode),
            decoders_for_source(&source_encode),
        );
    }

    Decoders {
        gstreamer: gstreamer_build_info(),
        decodings,
    }
}

pub fn decoder_factory_names(source_encode: &VideoEncodeType) -> Vec<String> {
    decoders_for_source(source_encode)
        .into_iter()
        .map(|decoder| decoder.name)
        .collect()
}

fn source_encode_key(source_encode: &VideoEncodeType) -> String {
    match source_encode {
        VideoEncodeType::Mjpg => "MJPG".to_string(),
        VideoEncodeType::H264 => "H264".to_string(),
        VideoEncodeType::H265 => "H265".to_string(),
        unsupported => format!("{unsupported:?}"),
    }
}

fn decoders_for_source(source_encode: &VideoEncodeType) -> Vec<EncoderInfo> {
    let caps = match source_encode {
        VideoEncodeType::Mjpg => gst::Caps::builder("image/jpeg").build(),
        VideoEncodeType::H264 => gst::Caps::builder("video/x-h264").build(),
        VideoEncodeType::H265 => gst::Caps::builder("video/x-h265").build(),
        _ => return vec![],
    };
    let mut decoders = Vec::new();
    for factory in
        gst::ElementFactory::factories_with_type(gst::ElementFactoryType::DECODER, gst::Rank::NONE)
            .iter()
    {
        if !factory.can_sink_any_caps(caps.as_ref()) {
            continue;
        }
        decoders.push(decoder_info(factory));
    }
    decoders.sort_by(|left, right| left.name.cmp(&right.name));
    let preferred = preferred_decoder_factory_name(source_encode);
    if let Some(index) = decoders
        .iter()
        .position(|decoder| decoder.name == preferred)
    {
        let preferred_decoder = decoders.remove(index);
        decoders.insert(0, preferred_decoder);
    }
    decoders
}

pub fn preferred_decoder_factory_name(source_encode: &VideoEncodeType) -> String {
    match source_encode {
        VideoEncodeType::Mjpg => "jpegdec".to_string(),
        VideoEncodeType::H264 => "avdec_h264".to_string(),
        VideoEncodeType::H265 => "avdec_h265".to_string(),
        unsupported => format!("{unsupported:?}"),
    }
}

fn gstreamer_build_info() -> GStreamerBuildInfo {
    let (major, minor, micro, nano) = gst::version();
    GStreamerBuildInfo {
        version: format!("{major}.{minor}.{micro}"),
        version_string: gst::version_string().to_string(),
        major,
        minor,
        micro,
        nano,
    }
}

fn decoder_info(factory: &gst::ElementFactory) -> EncoderInfo {
    let name = factory.name().to_string();
    let long_name = factory.longname().to_string();
    let description = factory.description().to_string();
    let rank = factory.rank();
    EncoderInfo {
        name: name.clone(),
        nick: long_name.clone(),
        blurb: Some(description.clone()).filter(|blurb| !blurb.is_empty()),
        docs_url: gst_element_docs_url(&name),
        factory: EncoderFactoryDetails {
            name: name.clone(),
            long_name,
            klass: factory.klass().to_string(),
            description,
            author: factory.author().to_string(),
            rank: rank.to_string(),
            rank_value: i32::from(rank),
        },
        plugin: factory.plugin().map(|plugin| EncoderPluginDetails {
            name: plugin.plugin_name().to_string(),
            description: plugin.description().to_string(),
            filename: plugin.filename().map(|path| path.display().to_string()),
            version: plugin.version().to_string(),
            license: plugin.license().to_string(),
            source: plugin.source().to_string(),
            release_date: plugin.release_date_string().map(|date| date.to_string()),
            package: plugin.package().to_string(),
            origin: plugin.origin().to_string(),
            is_loaded: plugin.is_loaded(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_includes_preferred_mjpg_h264_and_h265_decoders() {
        let _ = gst::init();
        let listing = decoders();
        assert!(!listing.gstreamer.version_string.is_empty());
        for (key, encode, preferred) in [
            ("MJPG", VideoEncodeType::Mjpg, "jpegdec"),
            ("H264", VideoEncodeType::H264, "avdec_h264"),
            ("H265", VideoEncodeType::H265, "avdec_h265"),
        ] {
            if gst::ElementFactory::find(preferred).is_none() {
                continue;
            }
            let found = listing
                .decodings
                .get(key)
                .unwrap_or_else(|| panic!("{key} listing"));
            assert!(
                found.iter().any(|decoder| decoder.name == preferred),
                "{preferred} listed under {key}"
            );
            assert_eq!(
                decoder_factory_names(&encode).first().map(String::as_str),
                Some(preferred)
            );
        }
    }
}
