pub mod decoders;
pub mod docs;
pub mod encoders;
pub mod encoding;
pub mod info;
pub mod utils;

#[cfg(all(test, target_os = "linux"))]
mod gsettings_test_env {
    #[used]
    #[unsafe(link_section = ".init_array")]
    static INIT: extern "C" fn() = init;

    extern "C" fn init() {
        // Headless CI has no `org.gnome.system.proxy` schema. A GStreamer plugin
        // that queries GSettings otherwise aborts the test process with SIGTRAP.
        // SAFETY: `.init_array` runs before `main`, so no other thread can be
        // reading the environment yet.
        unsafe {
            std::env::set_var("GSETTINGS_BACKEND", "memory");
        }
    }
}
