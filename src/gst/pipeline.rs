use crate::gst::gstreamer_runner;

#[derive(Clone)]
pub struct Pipeline {
    string: String,
}

impl Default for Pipeline {
    fn default() -> Self {
        Pipeline {
            string: "videotestsrc ! video/x-raw,width=640,height=480 ! videoconvert ! x264enc ! rtph264pay ! udpsink host=0.0.0.0 port=5600".to_string(),
        }
    }
}