use crate::settings;
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
use simple_error::SimpleError;

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
pub struct ResetSettings {
    all: Option<bool>,
}

#[derive(Apiv2Schema, Debug, Deserialize)]
pub struct ResetCameraControls {
    device: String,
}

#[derive(Apiv2Schema, Debug, Deserialize)]
pub struct XmlFileRequest {
    file: String,
}

use std::{ffi::OsStr, path::Path};

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
        _ => format!("File not found: {}", file_name),
    }
}

pub fn root(req: HttpRequest) -> HttpResponse {
    let filename = match req.match_info().query("filename") {
        "" | "index.html" => "index.html",
        "vue.js" => "vue.js",
        something => {
            //TODO: do that in load_file
            return HttpResponse::NotFound()
                .content_type("text/plain")
                .body(format!("Page does not exist: {}", something));
        }
    };
    let content = load_file(filename);
    let extension = Path::new(&filename)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let mime = actix_files::file_extension_to_mime(extension).to_string();
    return HttpResponse::Ok().content_type(mime).body(&content);
}

//TODO: change endpoint name to sources
#[api_v2_operation]
/// Provides list of all video sources, with controls and formats
pub async fn v4l() -> Json<Vec<ApiVideoSource>> {
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
pub fn v4l_post(json: web::Json<V4lControl>) -> HttpResponse {
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
/// Reset service settings
pub async fn reset_settings(query: web::Query<ResetSettings>) -> HttpResponse {
    if query.all.unwrap_or_default() {
        settings::manager::reset();
        stream_manager::start_default();
        return HttpResponse::Ok().finish();
    }

    return HttpResponse::NotAcceptable()
        .content_type("text/plain")
        .body("Missing argument for reset_settings.");
}

#[api_v2_operation]
/// Provide a list of all streams configured
pub async fn streams() -> Json<Vec<StreamStatus>> {
    let streams = stream_manager::streams();
    Json(streams)
}

#[api_v2_operation]
/// Create a video stream
pub fn streams_post(json: web::Json<PostStream>) -> HttpResponse {
    let json = json.into_inner();

    let video_source = match video_source::get_video_source(&json.source) {
        Ok(video_source) => video_source,
        Err(error) => {
            return HttpResponse::NotAcceptable()
                .content_type("text/plain")
                .body(format!("{:#?}", SimpleError::from(error).to_string()));
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
pub fn remove_stream(query: web::Query<RemoveStream>) -> HttpResponse {
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
/// Reset controls from a given camera source
pub fn camera_reset_controls(json: web::Json<ResetCameraControls>) -> HttpResponse {
    match video_source::reset_controls(&json.device) {
        Ok(_) => HttpResponse::Ok()
            .content_type("application/json")
            .body(serde_json::to_string_pretty(&stream_manager::streams()).unwrap()),
        Err(errors) => {
            let mut error: String = Default::default();
            errors.iter().enumerate().for_each(|(i, e)| {
                error
                    .push_str(format!("{}: {}\n", i + 1, SimpleError::from(e).to_string()).as_str())
            });
            let error = SimpleError::new(error);
            return HttpResponse::NotAcceptable()
                .content_type("text/plain")
                .body(format!(
                    "One or more controls were not reseted due to the following errors: \n{}",
                    error.to_string()
                ));
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
