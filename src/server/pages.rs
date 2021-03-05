use crate::stream::types::StreamInformation;
use crate::video::{
    types::{Control, Format, VideoSourceType},
    video_source,
    video_source::VideoSource,
    xml,
};
use actix_web::{web, HttpRequest, HttpResponse};
use log::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct V4LCamera {
    name: String,
    camera: String,
    formats: Vec<Format>,
    controls: Vec<Control>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct V4lControl {
    device: String,
    v4l_id: u64,
    value: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PostStream {
    name: String,
    device_path: String,
    stream_information: StreamInformation,
}

#[derive(Debug, Deserialize)]
pub struct XmlFileRequest {
    file: String,
}

pub fn root(req: HttpRequest) -> HttpResponse {
    let index = std::include_str!("../html/index.html");
    let vue = std::include_str!("../html/vue.js");
    let path = match req.match_info().query("filename") {
        "" | "index.html" => index,
        "vue.js" => vue,
        something => {
            return HttpResponse::NotFound()
                .content_type("text/plain")
                .body(format!("Page does not exist: {}", something));
        }
    };
    HttpResponse::Ok().content_type("text/html").body(path)
}

pub fn v4l(req: HttpRequest) -> HttpResponse {
    debug!("{:#?}", req);

    let cameras = video_source::cameras_available();
    let cameras: Vec<V4LCamera> = cameras
        .iter()
        .map(|cam| {
            if let VideoSourceType::Local(cam) = cam {
                return Some(V4LCamera {
                    name: cam.name().clone(),
                    camera: cam.source_string().clone(),
                    formats: cam.formats(),
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

pub fn v4l_post(req: HttpRequest, json: web::Json<V4lControl>) -> HttpResponse {
    debug!("{:#?}{:?}", req, json);
    let control = json.into_inner();
    let answer = video_source::set_control(&control.device, control.v4l_id, control.value);
    if answer.is_ok() {
        return HttpResponse::Ok().finish();
    };

    return HttpResponse::NotAcceptable()
        .content_type("text/plain")
        .body(format!("{:#?}", answer.err().unwrap()));
}

pub fn streams(req: HttpRequest) -> HttpResponse {
    debug!("{:#?}", req);
    use crate::stream::manager as stream_manager;
    let streams = stream_manager::streams();
    HttpResponse::Ok()
        .content_type("text/plain")
        .body(serde_json::to_string_pretty(&streams).unwrap())
}

pub fn streams_post(req: HttpRequest, json: web::Json<PostStream>) -> HttpResponse {
    debug!("{:#?}{:?}", req, json);
    let json = json.into_inner();
    //json.
    //TODO: Move stream manager to absolute scope, check others places
    use crate::stream::manager as stream_manager;
    use crate::video_stream::types::VideoAndStreamInformation;

    let video_source = match video_source::get_video_source(&json.device_path) {
        Ok(video_source) => video_source,
        Err(error) => {
            return HttpResponse::NotAcceptable()
                .content_type("text/plain")
                .body(format!("{:#?}", error.to_string()));
        }
    };

    match stream_manager::add_stream_and_start(VideoAndStreamInformation {
        name: json.name,
        stream_information: json.stream_information,
        video_source,
    }) {
        Ok(_) => HttpResponse::Ok()
            .content_type("text/plain")
            .body(serde_json::to_string_pretty(&stream_manager::streams()).unwrap()),
        Err(error) => {
            return HttpResponse::NotAcceptable()
                .content_type("text/plain")
                .body(format!("{:#?}", error.to_string()));
        }
    }
}

pub fn xml(web::Query(xml_file_request): web::Query<XmlFileRequest>) -> HttpResponse {
    debug!("{:#?}", xml_file_request);
    let cameras = video_source::cameras_available();
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
