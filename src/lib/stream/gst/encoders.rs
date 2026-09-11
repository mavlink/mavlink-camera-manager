use std::collections::BTreeMap;

use paperclip::actix::Apiv2Schema;
use serde::Serialize;
use tracing::{error, warn};

use gst::prelude::*;

use super::docs::gst_element_docs_url;
use super::encoding::CompressedEncoding;
use super::utils::encoder_factory_can_encode;

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct Encoders {
    pub gstreamer: GStreamerBuildInfo,
    pub encodings: BTreeMap<String, Vec<EncoderInfo>>,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct GStreamerBuildInfo {
    pub version: String,
    pub version_string: String,
    pub major: u32,
    pub minor: u32,
    pub micro: u32,
    pub nano: u32,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct EncoderInfo {
    pub name: String,
    pub nick: String,
    pub blurb: Option<String>,
    pub docs_url: Option<String>,
    pub factory: EncoderFactoryDetails,
    pub plugin: Option<EncoderPluginDetails>,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct EncoderFactoryDetails {
    pub name: String,
    pub long_name: String,
    pub klass: String,
    pub description: String,
    pub author: String,
    pub rank: String,
    pub rank_value: i32,
}

#[derive(Apiv2Schema, Clone, Debug, Serialize)]
pub struct EncoderPluginDetails {
    pub name: String,
    pub description: String,
    pub filename: Option<String>,
    pub version: String,
    pub license: String,
    pub source: String,
    pub release_date: Option<String>,
    pub package: String,
    pub origin: String,
    pub is_loaded: bool,
}

pub fn encoders() -> Encoders {
    if let Err(error) = gst::init() {
        error!("{error}");
        return Encoders {
            gstreamer: gstreamer_build_info(),
            encodings: BTreeMap::new(),
        };
    }

    let mut encodings = BTreeMap::new();
    for encoding in super::encoding::encodings() {
        encodings.insert(encoding.encode_key().to_string(), encoders_for(*encoding));
    }

    Encoders {
        gstreamer: gstreamer_build_info(),
        encodings,
    }
}

pub fn encoder_factory_names(encoding: &dyn CompressedEncoding) -> Vec<String> {
    encoders_for(encoding)
        .into_iter()
        .map(|encoder| encoder.name)
        .collect()
}

fn encoders_for(encoding: &dyn CompressedEncoding) -> Vec<EncoderInfo> {
    let caps = gst::Caps::builder(encoding.caps_mime()).build();
    let mut encoders = Vec::new();
    // VIDEO_ENCODER includes image encoders (jpegenc) as well as video.
    for factory in gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::VIDEO_ENCODER,
        gst::Rank::NONE,
    )
    .iter()
    {
        if !factory.can_src_any_caps(caps.as_ref()) {
            continue;
        }
        let name = factory.name().to_string();
        if let Err(error) = encoder_factory_can_encode(encoding, &name) {
            warn!(
                "Dropped {} encoder {name}: {error:#}",
                encoding.encode_key()
            );
            continue;
        }
        encoders.push(encoder_info(factory));
    }
    encoders.sort_by(|left, right| left.name.cmp(&right.name));
    let preferred = encoding.preferred_encoder_factory();
    if let Some(index) = encoders
        .iter()
        .position(|encoder| encoder.name == preferred)
    {
        let preferred_encoder = encoders.remove(index);
        encoders.insert(0, preferred_encoder);
    }
    encoders
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

fn encoder_info(factory: &gst::ElementFactory) -> EncoderInfo {
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
    use crate::stream::gst::encoding::{H264, Mjpg};

    #[test]
    fn listing_includes_preferred_h264_and_mjpg_factories() {
        let _ = gst::init();
        let listing = encoders();
        assert!(!listing.gstreamer.version_string.is_empty());
        let h264 = listing
            .encodings
            .get("H264")
            .expect("H264")
            .iter()
            .find(|encoder| encoder.name == "x264enc")
            .expect("x264enc");
        assert_eq!(h264.factory.name, "x264enc");
        assert_eq!(encoder_factory_names(&H264)[0], "x264enc");
        let mjpg = listing
            .encodings
            .get("MJPG")
            .expect("MJPG")
            .iter()
            .find(|encoder| encoder.name == "jpegenc")
            .expect("jpegenc");
        assert_eq!(mjpg.factory.name, "jpegenc");
        assert_eq!(encoder_factory_names(&Mjpg)[0], "jpegenc");
    }
}
