use glib::ParamSpec;

pub fn mutable_in_playing(param_spec: &ParamSpec) -> bool {
    param_spec.flags().contains(gst::PARAM_FLAG_MUTABLE_PLAYING)
}

pub fn requires_restart(param_spec: &ParamSpec) -> bool {
    let flags = param_spec.flags();
    flags.contains(glib::ParamFlags::WRITABLE)
        && !flags.contains(gst::PARAM_FLAG_MUTABLE_PLAYING)
        && !flags.contains(gst::PARAM_FLAG_MUTABLE_PAUSED)
        && !flags.contains(gst::PARAM_FLAG_MUTABLE_READY)
}
