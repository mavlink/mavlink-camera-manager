use crate::video;
use crate::video::types::FrameSize;
use actix_web::{web, HttpRequest, HttpResponse};
use log::*;
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
    //TODO: check includes vs types
    resolutions: Vec<FrameSize>,
    controls: Vec<video::types::Control>,
}

pub fn v4l(req: HttpRequest) -> HttpResponse {
    debug!("{:#?}", req);
    use video::types::VideoSourceType; //TODO: maybe moving to video_source?
    use video::video_source::VideoSource;

    let cameras = video::video_source::cameras_available();
    let cameras: Vec<V4LCamera> = cameras
        .iter()
        .map(|cam| {
            if let VideoSourceType::Usb(cam) = cam {
                return Some(V4LCamera {
                    name: cam.name().clone(),
                    camera: cam.source_string().clone(),
                    resolutions: cam.resolutions(),
                    controls: cam.controls(),
                });
            } else {
                return None;
            }
        })
        .take_while(|cam| cam.is_some())
        .map(|cam| cam.unwrap())
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
    debug!("{:#?}{:?}", req, json);
    //TODO: check all uses here in this file
    use video::video_source;
    let control = json.into_inner();

    let answer = video_source::set_control(&control.device, control.v4l_id, control.value);

    if answer.is_ok() {
        return HttpResponse::Ok().finish();
    };

    return HttpResponse::NotAcceptable()
        .content_type("text/plain")
        .body(format!("{:#?}", answer.err().unwrap()));
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
        .body(format!(
            "File for {} does not exist.",
            xml_file_request.file
        ));
}
