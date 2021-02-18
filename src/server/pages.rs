use actix_web::{HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use crate::video;

pub fn root(_req: HttpRequest) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain")
        .body(format!("Hello !"))
}

#[derive(Debug, Serialize)]
struct V4LCamera {
    name: String,
    camera: String,
    resolutions: Vec<video::video_source::FrameSize>,
    controls: Vec<video::types::Control>,
}

pub fn v4l(_req: HttpRequest) -> HttpResponse {
    use video::video_source::{VideoSource, VideoSourceType};

    let cameras = video::video_source::cameras_available();
    let cameras: Vec<V4LCamera> = cameras.iter()
    .map(|cam| if let VideoSourceType::Usb(cam) = cam {
        return Some(cam);
    } else {
        return None;
    })
    .take_while(|cam| cam.is_some())
    .map(|cam| cam.unwrap())
        .map(|cam| V4LCamera {
            name: cam.name().clone(),
            camera: cam.source_string().clone(),
            resolutions: cam.resolutions(),
            controls: cam.controls(),
        })
        .collect();

    HttpResponse::Ok()
        .content_type("text/plain")
        .body(serde_json::to_string_pretty(&cameras).unwrap())
}