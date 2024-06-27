#[macro_use]
extern crate lazy_static;
extern crate paperclip;
extern crate tracing;

#[macro_use]
pub mod helper;

pub mod cli;
pub mod custom;
pub mod logger;
pub mod mavlink;
pub mod network;
pub mod server;
pub mod settings;
pub mod stream;
pub mod video;
pub mod video_stream;
