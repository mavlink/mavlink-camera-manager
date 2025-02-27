use std::io::prelude::*;

use actix_web::{
    http::header,
    rt,
    web::{self, Json},
    HttpRequest, HttpResponse,
};
use paperclip::actix::{api_v2_operation, Apiv2Schema, CreatedJson};
use serde::{Deserialize, Serialize};
use tracing::*;
use validator::Validate;

use crate::{
    controls::types::Control,
    helper,
    server::error::{Error, Result},
    settings,
    stream::{gst as gst_stream, manager as stream_manager, types::StreamInformation},
    video::{
        types::{Format, VideoSourceType},
        video_source::{self, VideoSource, VideoSourceFormats},
        xml,
    },
    video_stream::types::VideoAndStreamInformation,
};

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

#[derive(Apiv2Schema, Debug, Deserialize)]
pub struct SdpFileRequest {
    source: String,
}

#[derive(Apiv2Schema, Debug, Deserialize, Validate)]
pub struct ThumbnailFileRequest {
    source: String,
    /// The Quality level (a percentage value as an integer between 1 and 100) is inversely proportional to JPEG compression level, which means the higher, the best.
    #[validate(range(min = 1, max = 100))]
    quality: Option<u8>,
    /// Target height of the thumbnail. The value should be an integer between 1 and 1080 (because of memory constraints).
    #[validate(range(min = 1, max = 1080))]
    target_height: Option<u16>,
}

#[derive(Apiv2Schema, Serialize, Debug)]
pub struct Development {
    number_of_tasks: usize,
}

#[derive(Apiv2Schema, Serialize, Debug)]
pub struct Info {
    /// Name of the program
    name: String,
    /// Version/tag
    version: String,
    /// Git SHA
    sha: String,
    build_date: String,
    /// Authors name
    authors: String,
    /// Unstable field for custom development
    development: Development,
}

impl Info {
    pub fn new() -> Self {
        Self {
            name: env!("CARGO_PKG_NAME").into(),
            version: env!("VERGEN_GIT_SEMVER").into(),
            sha: env!("VERGEN_GIT_SHA_SHORT").into(),
            build_date: env!("VERGEN_BUILD_DATE").into(),
            authors: env!("CARGO_PKG_AUTHORS").into(),
            development: Development {
                number_of_tasks: helper::threads::process_task_counter(),
            },
        }
    }
}

#[derive(Apiv2Schema, Deserialize, Debug)]
pub struct AuthenticateOnvifDeviceRequest {
    /// Onvif Device UUID, obtained via `/onvif/devices` get request
    device_uuid: uuid::Uuid,
    /// Username for the Onvif Device
    username: String,
    /// Password for the Onvif Device
    password: String,
}

#[derive(Apiv2Schema, Deserialize, Debug)]
pub struct UnauthenticateOnvifDeviceRequest {
    /// Onvif Device UUID, obtained via `/onvif/devices` get request
    device_uuid: uuid::Uuid,
}

use std::{ffi::OsStr, path::Path};

use include_dir::{include_dir, Dir};

static WEBRTC_DIST: Dir<'_> = include_dir!("src/lib/stream/webrtc/frontend/dist");

fn load_file(file_name: &str) -> String {
    if file_name.starts_with("webrtc/") {
        return load_webrtc(file_name);
    }

    // Load files at runtime only in debug builds
    if cfg!(debug_assertions) {
        let html_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/html/");
        let mut file = std::fs::File::open(html_path.join(file_name)).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        return contents;
    }

    match file_name {
        "" | "index.html" => std::include_str!("../../html/index.html").into(),
        "vue.js" => std::include_str!("../../html/vue.js").into(),
        _ => format!("File not found: {file_name:?}"),
    }
}

fn load_webrtc(filename: &str) -> String {
    let filename = filename.trim_start_matches("webrtc/");
    let file = WEBRTC_DIST.get_file(filename).unwrap();
    let content = file.contents_utf8().unwrap();
    content.into()
}

#[api_v2_operation]
pub fn root(req: HttpRequest) -> Result<HttpResponse> {
    let filename = match req.match_info().query("filename") {
        "" | "index.html" => "index.html",
        "vue.js" => "vue.js",

        webrtc_file if webrtc_file.starts_with("webrtc/") => webrtc_file,

        something => {
            //TODO: do that in load_file
            return Err(Error::NotFound(format!(
                "Page does not exist: {something:?}"
            )));
        }
    };
    let content = load_file(filename);
    let extension = Path::new(&filename)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let mime = actix_files::file_extension_to_mime(extension).to_string();

    Ok(HttpResponse::Ok().content_type(mime).body(content))
}

#[api_v2_operation]
/// Provide information about the running service
/// There is no stable API guarantee for the development field
pub async fn info() -> Result<CreatedJson<Info>> {
    Ok(CreatedJson(Info::new()))
}

//TODO: change endpoint name to sources
#[api_v2_operation]
/// Provides list of all video sources, with controls and formats
pub async fn v4l() -> Result<Json<Vec<ApiVideoSource>>> {
    let cameras = video_source::cameras_available().await;

    use futures::stream::{self, StreamExt};

    let cameras: Vec<ApiVideoSource> = stream::iter(cameras)
        .then(|cam| async {
            match cam {
                VideoSourceType::Local(local) => ApiVideoSource {
                    name: local.name().clone(),
                    source: local.source_string().to_string(),
                    formats: local.formats().await,
                    controls: local.controls(),
                },
                VideoSourceType::Gst(gst) => ApiVideoSource {
                    name: gst.name().clone(),
                    source: gst.source_string().to_string(),
                    formats: gst.formats().await,
                    controls: gst.controls(),
                },
                VideoSourceType::Onvif(onvif) => ApiVideoSource {
                    name: onvif.name().clone(),
                    source: onvif.source_string().to_string(),
                    formats: onvif.formats().await,
                    controls: onvif.controls(),
                },
                VideoSourceType::Redirect(redirect) => ApiVideoSource {
                    name: redirect.name().clone(),
                    source: redirect.source_string().to_string(),
                    formats: redirect.formats().await,
                    controls: redirect.controls(),
                },
            }
        })
        .collect()
        .await;

    Ok(Json(cameras))
}

#[api_v2_operation]
/// Change video control for a specific source
pub async fn v4l_post(json: web::Json<V4lControl>) -> Result<HttpResponse> {
    let control = json.into_inner();
    video_source::set_control(&control.device, control.v4l_id, control.value)
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok().finish())
}

#[api_v2_operation]
/// Reset service settings
pub async fn reset_settings(query: web::Query<ResetSettings>) -> Result<HttpResponse> {
    if query.all.unwrap_or_default() {
        settings::manager::reset().await;
        stream_manager::start_default()
            .await
            .map_err(|error| Error::Internal(format!("{error:?}")))?;

        return Ok(HttpResponse::Ok().finish());
    }

    Err(Error::Internal(
        "Missing argument for reset_settings.".to_string(),
    ))
}

#[api_v2_operation]
/// Restart streams
pub async fn restart_streams(query: web::Query<ResetSettings>) -> Result<HttpResponse> {
    if query.all.unwrap_or_default() {
        stream_manager::start_default()
            .await
            .map_err(|error| Error::Internal(format!("{error:?}")))?;

        return Ok(HttpResponse::Ok().finish());
    }

    Err(Error::Internal(
        "Missing argument for restart_streams.".to_string(),
    ))
}

#[api_v2_operation]
/// Provide a list of all streams configured
pub async fn streams() -> Result<HttpResponse> {
    let streams = stream_manager::streams()
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    let json = serde_json::to_string_pretty(&streams)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
/// Create a video stream
pub async fn streams_post(json: web::Json<PostStream>) -> Result<HttpResponse> {
    let json = json.into_inner();

    let video_source = video_source::get_video_source(&json.source)
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    stream_manager::add_stream_and_start(VideoAndStreamInformation {
        name: json.name,
        stream_information: json.stream_information,
        video_source,
    })
    .await
    .map_err(|error| Error::Internal(format!("{error:?}")))?;

    // Return the new streams available
    streams()
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))
}

#[api_v2_operation]
/// Remove a desired stream
pub fn remove_stream(query: web::Query<RemoveStream>) -> Result<HttpResponse> {
    stream_manager::remove_stream_by_name(&query.name)
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    let streams = stream_manager::streams()
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    let json = serde_json::to_string_pretty(&streams)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
/// Reset controls from a given camera source
pub fn camera_reset_controls(json: web::Json<ResetCameraControls>) -> Result<HttpResponse> {
    if let Err(errors) = video_source::reset_controls(&json.device).await {
        let mut error: String = Default::default();
        errors
            .iter()
            .enumerate()
            .for_each(|(i, e)| error.push_str(&format!("{}: {e}\n", i + 1)));
        return Err(Error::Internal(format!(
            "One or more controls were not reseted due to the following errors: \n{error:#?}",
        )));
    }

    let streams = stream_manager::streams()
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    let json = serde_json::to_string_pretty(&streams)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
/// Provides a xml description file that contains information for a specific device, based on: https://mavlink.io/en/services/camera_def.html
pub fn xml(xml_file_request: web::Query<XmlFileRequest>) -> Result<HttpResponse> {
    debug!("{xml_file_request:#?}");
    let cameras = video_source::cameras_available().await;
    let camera = cameras
        .iter()
        .find(|source| source.inner().source_string() == xml_file_request.file);

    let Some(camera) = camera else {
        return Err(Error::NotFound(format!(
            "File for {} does not exist.",
            xml_file_request.file
        )));
    };

    let xml = xml::from_video_source(camera.inner()).map_err(|error| {
        Error::Internal(format!(
            "Failed getting XML file {}: {error:?}",
            xml_file_request.file
        ))
    })?;

    Ok(HttpResponse::Ok().content_type("text/xml").body(xml))
}

#[api_v2_operation]
/// Provides a sdp description file that contains information for a specific stream, based on: [RFC 8866](https://www.rfc-editor.org/rfc/rfc8866.html)
pub fn sdp(sdp_file_request: web::Query<SdpFileRequest>) -> Result<HttpResponse> {
    debug!("{sdp_file_request:#?}");

    let sdp = stream_manager::get_first_sdp_from_source(sdp_file_request.source.clone())
        .await
        .map_err(|error| {
            Error::Internal(format!(
                "Failed to get SDP file for {:?}. Reason: {error:?}",
                sdp_file_request.source
            ))
        })?;

    let sdp_text = sdp
        .as_text()
        .map_err(|error| Error::Internal(format!("Failed to convert SDP to text: {error:?}")))?;

    Ok(HttpResponse::Ok().content_type("text/plain").body(sdp_text))
}

#[api_v2_operation]
/// Provides a thumbnail file of the given source
pub async fn thumbnail(
    thumbnail_file_request: web::Query<ThumbnailFileRequest>,
) -> Result<HttpResponse> {
    // Ideally, we should be using `actix_web_validator::Query` instead of `web::Query`,
    // but because paperclip (at least until 0.8) is using `actix-web-validator 3.x`,
    // and `validator 0.14`, the newest api needed to use it along #[api_v2_operation]
    // wasn't implemented yet, it doesn't compile.
    // To workaround this, we are manually calling the validator here, using actix to
    // automatically handle the validation error for us as it normally would.
    // TODO: update this function to use `actix_web_validator::Query` directly and get
    // rid of this workaround.
    if let Err(errors) = thumbnail_file_request.validate() {
        warn!("Failed validating ThumbnailFileRequest. Reason: {errors:?}");
        return Ok(actix_web::ResponseError::error_response(
            &actix_web_validator::Error::from(errors),
        ));
    }

    let source = thumbnail_file_request.source.clone();
    let quality = thumbnail_file_request.quality.unwrap_or(70u8);
    let target_height = thumbnail_file_request.target_height.map(|v| v as u32);

    match stream_manager::get_jpeg_thumbnail_from_source(source, quality, target_height).await {
        Some(Ok(image)) => Ok(HttpResponse::Ok().content_type("image/jpeg").body(image)),
        None => Ok(HttpResponse::NotFound()
            .content_type("text/plain")
            .body(format!(
                "Thumbnail not found for source {:?}.",
                thumbnail_file_request.source
            ))),
            Some(Err(error)) => Ok(HttpResponse::ServiceUnavailable()
            .reason("Thumbnail temporarily unavailable")
            .insert_header((header::RETRY_AFTER, 10))
            .content_type("text/plain")
            .body(format!(
            "Thumbnail for source {:?} is temporarily unavailable. Try again later. Details: {error:?}",
            thumbnail_file_request.source
        ))),
    }
}

#[api_v2_operation]
/// Provides information related to all gst plugins available for camera manager
pub async fn gst_info() -> Result<HttpResponse> {
    let gst_info = gst_stream::info::Info::default();

    let json = serde_json::to_string_pretty(&gst_info)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
/// Provides a access point for the service log
pub async fn log(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse> {
    let (response, mut session, _stream) =
        actix_ws::handle(&req, stream).map_err(|error| Error::Internal(format!("{error:?}")))?;

    rt::spawn(async move {
        let (mut receiver, history) = crate::logger::manager::HISTORY.lock().unwrap().subscribe();

        for message in history {
            if session.text(message).await.is_err() {
                return;
            }
        }

        while let Ok(message) = receiver.recv().await {
            if session.text(message).await.is_err() {
                return;
            }
        }
    });

    Ok(response)
}

#[api_v2_operation]
pub async fn onvif_devices() -> Result<HttpResponse> {
    let onvif_devices = crate::controls::onvif::manager::Manager::onvif_devices().await;

    let json = serde_json::to_string_pretty(&onvif_devices)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
pub async fn authenticate_onvif_device(
    query: web::Query<AuthenticateOnvifDeviceRequest>,
) -> Result<HttpResponse> {
    crate::controls::onvif::manager::Manager::register_credentials(
        query.device_uuid,
        Some(onvif::soap::client::Credentials {
            username: query.username.clone(),
            password: query.password.clone(),
        }),
    )
    .await
    .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok().finish())
}

#[api_v2_operation]
pub async fn unauthenticate_onvif_device(
    query: web::Query<UnauthenticateOnvifDeviceRequest>,
) -> Result<HttpResponse> {
    crate::controls::onvif::manager::Manager::register_credentials(query.device_uuid, None)
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok().finish())
}
