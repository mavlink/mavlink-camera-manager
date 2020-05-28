use regex::Regex;

mod helper;
mod mavlink_camera_information;

use actix_web::{web, App, HttpRequest, HttpServer};

#[cfg(feature = "gst")]
mod gst;

#[cfg(feature = "gst")]
use std::thread;

#[cfg(feature = "rtsp")]
pub fn start_rtsp_server(pipeline: &str, port: u16) {
    thread::spawn({
        let mut rtsp = gst::rtsp_server::RTSPServer::default();
        rtsp.set_pipeline(pipeline);
        rtsp.set_port(port);
        move || loop {
            rtsp.run_loop();
        }
    });
}

#[cfg(not(feature = "rtsp"))]
pub fn start_rtsp_server(_pipeline: &str, _port: u16) {}

#[cfg(feature = "gst")]
pub fn start_pipeline(pipeline: &str) {
    thread::spawn({
        let mut pipeline_runner = gst::pipeline_runner::PipelineRunner::default();
        pipeline_runner.set_pipeline(pipeline);
        move || loop {
            pipeline_runner.run_loop();
        }
    });
}

#[cfg(not(feature = "gst"))]
pub fn start_pipeline(_pipeline: &str) {}

mod settings;

fn main() {}
