use crate::video;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};

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

pub fn v4l(req: HttpRequest) -> HttpResponse {
    //println!("{:#?} {:#?} {:#?}", req.method(), req.app_data::<V4lControl>(), json);
    use video::video_source::{VideoSource, VideoSourceType};

    let cameras = video::video_source::cameras_available();
    let cameras: Vec<V4LCamera> = cameras
        .iter()
        .map(|cam| {
            if let VideoSourceType::Usb(cam) = cam {
                return Some(cam);
            } else {
                return None;
            }
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

#[derive(Debug, Deserialize, Serialize)]
pub struct V4lControl {
    device: String,
    v4l_id: u64,
    value: i64,
}

pub fn v4l_post(req: HttpRequest, json: web::Json<V4lControl>) -> HttpResponse {
    let v4l_control = json.into_inner();
    println!("{:#?}", v4l_control);
    return HttpResponse::Ok()
        .content_type("text/xml")
        .body(serde_json::to_string_pretty(&v4l_control).unwrap());
}

#[derive(Deserialize)]
pub struct XmlFileRequest {
    file: String,
}

pub fn xml(web::Query(xml_file_request): web::Query<XmlFileRequest>) -> HttpResponse {
    use video::video_source::VideoSource;
    use video::xml;
    let cameras = video::video_source::cameras_available();
    let camera = cameras
        .iter()
        .find(|source| source.inner().source_string() == &xml_file_request.file);

    if let Some(camera) = camera {
        return HttpResponse::Ok()
            .content_type("text/xml")
            .body(xml::from_video_source(&camera.inner()));
    }
    return HttpResponse::NotFound()
        .content_type("text/plain")
        .body(format!("File for {} does not exist.", xml_file_request.file));
}
