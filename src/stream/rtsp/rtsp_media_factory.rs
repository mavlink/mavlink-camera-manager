/// /References:
/// 1 - https://gitlab.freedesktop.org/slomo/rtp-rapid-sync-example
/// 2 - https://github.com/gtk-rs/gtk3-rs/blob/master/examples/list_box_model/row_data/imp.rs
mod imp {
    use std::sync::{Arc, Mutex};

    use once_cell::sync::Lazy;

    use gst::{
        glib::{self, subclass::prelude::*, *},
        prelude::*,
    };
    use gst_rtsp_server::subclass::prelude::*;

    // The actual data structure that stores our values. This is not accessible
    // directly from the outside.
    #[derive(Default)]
    pub struct Factory {
        bin: Arc<Mutex<gst::Bin>>,
    }

    // Basic declaration of our type for the GObject type system
    #[glib::object_subclass]
    impl ObjectSubclass for Factory {
        const NAME: &'static str = "RTSPMediaFactoryFromBin";
        type Type = super::Factory;
        type ParentType = gst_rtsp_server::RTSPMediaFactory;
    }

    // The ObjectImpl trait provides the setters/getters for GObject properties.
    // Here we need to provide the values that are internally stored back to the
    // caller, or store whatever new value the caller is providing.
    //
    // This maps between the GObject properties and our internal storage of the
    // corresponding values of the properties.
    impl ObjectImpl for Factory {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
                vec![glib::ParamSpecObject::builder::<gst::Bin>("bin")
                    .construct_only()
                    .build()]
            });

            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "bin" => {
                    let bin = value.get().unwrap();
                    self.bin.set(bin);
                }
                _ => unimplemented!(),
            }
        }
    }

    impl RTSPMediaFactoryImpl for Factory {
        // Create the custom stream producer bin.
        fn create_element(&self, _url: &gst_rtsp::RTSPUrl) -> Option<gst::Element> {
            let bin = self.bin.lock().unwrap();
            bin.debug_to_dot_file_with_ts(gst::DebugGraphDetails::all(), "rtsp-bin-created");

            Some(bin.to_owned().upcast())
        }
    }
}

gst::glib::wrapper! {
    pub struct Factory(ObjectSubclass<imp::Factory>) @extends gst_rtsp_server::RTSPMediaFactory;
}

// Trivial constructor for the media factory.
impl Factory {
    pub fn new(bin: gst::Bin) -> Self {
        gst::glib::Object::builder().property("bin", bin).build()
    }
}
