use crate::stream::{
    manager as stream_manager,
    types::{StreamInformation, StreamStatus},
};
use crate::video::{
    types::{Control, Format, VideoSourceType},
    video_source,
    video_source::VideoSource,
    xml,
};
use crate::video_stream::types::VideoAndStreamInformation;
use actix_web::{
    web::{self, Json},
    HttpRequest, HttpResponse,
};
use log::*;
use paperclip::actix::{api_v2_operation, Apiv2Schema};
use serde::{Deserialize, Serialize};

use std::io::prelude::*;

#[derive(Apiv2Schema, Debug, Serialize)]
pub struct ApiVideoSource {
    name: String,
    source: String,
    formats: Vec<Format>,
    controls: Vec<Control>,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct V4lControl {
    device: String,
    v4l_id: u64,
    value: i64,
}

#[derive(Apiv2Schema, Debug, Deserialize, Serialize)]
pub struct PostStream {
    name: String,
    source: String,
    stream_information: StreamInformation,
}

#[derive(Apiv2Schema, Debug, Deserialize)]
pub struct RemoveStream {
    name: String,
}

#[derive(Apiv2Schema, Debug, Deserialize)]
pub struct XmlFileRequest {
    file: String,
}

pub fn load_file(file_name: &str) -> String {
    // Load files at runtime only in debug builds
    if cfg!(debug_assertions) {
        let html_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/html/");
        let mut file = std::fs::File::open(html_path.join(file_name)).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        return contents;
    }

    match file_name {
        "" | "index.html" => std::include_str!("../html/index.html").into(),
        "vue.js" => std::include_str!("../html/vue.js").into(),
        "webrtc/index.html" => std::include_str!("../html/webrtc/index.html").into(),
        "webrtc/webrtc.js" => std::include_str!("../html/webrtc/webrtc.js").into(),
        _ => format!("File not found: {}", file_name),
    }
}

pub fn root(req: HttpRequest) -> HttpResponse {
    let filename = req.match_info().query("filename");
    let path = match filename {
        "" | "index.html" => load_file("index.html"),
        "vue.js" => load_file("vue.js"),
        "webrtc/" | "webrtc/index.html" => load_file("webrtc/index.html"),
        "webrtc/webrtc.js" => load_file("webrtc/webrtc.js"),
        something => {
            //TODO: do that in load_file
            return HttpResponse::NotFound()
                .content_type("text/plain")
                .body(format!("Page does not exist: {}", something));
        }
    };
    if filename.ends_with(".js") {
        return HttpResponse::Ok()
            .content_type("text/javascript")
            .body(path);
    }
    if filename.ends_with(".css") {
        return HttpResponse::Ok().content_type("text/css").body(path);
    }
    return HttpResponse::Ok().content_type("text/html").body(path);
}

//TODO: change endpoint name to sources
#[api_v2_operation]
/// Provides list of all video sources, with controls and formats
pub async fn v4l(req: HttpRequest) -> Json<Vec<ApiVideoSource>> {
    debug!("{:#?}", req);

    let cameras = video_source::cameras_available();
    let cameras: Vec<ApiVideoSource> = cameras
        .iter()
        .map(|cam| match cam {
            VideoSourceType::Local(cam) => ApiVideoSource {
                name: cam.name().clone(),
                source: cam.source_string().to_string(),
                formats: cam.formats(),
                controls: cam.controls(),
            },
            VideoSourceType::Gst(gst) => ApiVideoSource {
                name: gst.name().clone(),
                source: gst.source_string().to_string(),
                formats: gst.formats(),
                controls: gst.controls(),
            },
            VideoSourceType::Redirect(redirect) => ApiVideoSource {
                name: redirect.name().clone(),
                source: redirect.source_string().to_string(),
                formats: redirect.formats(),
                controls: redirect.controls(),
            },
        })
        .collect();

    Json(cameras)
}

#[api_v2_operation]
/// Change video control for a specific source
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

#[api_v2_operation]
/// Provide a list of all streams configured
pub async fn streams(req: HttpRequest) -> Json<Vec<StreamStatus>> {
    debug!("{:#?}", req);
    let streams = stream_manager::streams();
    Json(streams)
}

#[api_v2_operation]
/// Create a video stream
pub fn streams_post(req: HttpRequest, json: web::Json<PostStream>) -> HttpResponse {
    debug!("{:#?}{:?}", req, json);
    let json = json.into_inner();
    //json.

    let video_source = match video_source::get_video_source(&json.source) {
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
            .content_type("application/json")
            .body(serde_json::to_string_pretty(&stream_manager::streams()).unwrap()),
        Err(error) => {
            return HttpResponse::NotAcceptable()
                .content_type("text/plain")
                .body(format!("{:#?}", error.to_string()));
        }
    }
}

#[api_v2_operation]
/// Remove a desired stream
pub fn remove_stream(req: HttpRequest, query: web::Query<RemoveStream>) -> HttpResponse {
    debug!("{:#?}{:?}", req, query);

    match stream_manager::remove_stream(&query.name) {
        Ok(_) => HttpResponse::Ok()
            .content_type("application/json")
            .body(serde_json::to_string_pretty(&stream_manager::streams()).unwrap()),
        Err(error) => {
            return HttpResponse::NotAcceptable()
                .content_type("text/plain")
                .body(format!("{:#?}", error.to_string()));
        }
    }
}

#[api_v2_operation]
/// Provides a xml description file that contains information for a specific device, based on: https://mavlink.io/en/services/camera_def.html
pub fn xml(xml_file_request: web::Query<XmlFileRequest>) -> HttpResponse {
    debug!("{:#?}", xml_file_request);
    let cameras = video_source::cameras_available();
    let camera = cameras
        .iter()
        .find(|source| source.inner().source_string() == xml_file_request.file);

    if let Some(camera) = camera {
        return HttpResponse::Ok()
            .content_type("text/xml")
            .body(xml::from_video_source(camera.inner()));
    }
    return HttpResponse::NotFound()
        .content_type("text/plain")
        .body(format!(
            "File for {} does not exist.",
            xml_file_request.file
        ));
}
