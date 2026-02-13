use std::{ffi::OsStr, path::Path};

use actix_web::{
    http::header,
    rt,
    web::{self, Json},
    HttpRequest, HttpResponse,
};
use actix_ws::Message;
use futures::StreamExt;
use paperclip::actix::{api_v2_operation, CreatedJson};
use serde_json::json;
use tokio::time::Duration;
use tracing::*;

use mcm_api::v1::{
    server::{
        ApiVideoSource, AuthenticateOnvifDeviceRequest, BlockSource, Development, Info, PostStream,
        RemoveStream, ResetCameraControls, ResetSettings, SdpFileRequest, ThumbnailFileRequest,
        UnauthenticateOnvifDeviceRequest, UnblockSource, V4lControl, XmlFileRequest,
    },
    stats::{SetLevelRequest, SetWindowSizeRequest, SnapshotQuery, SnapshotWsQuery},
    stream::VideoAndStreamInformation,
    video::VideoSourceType,
};

use crate::{
    helper::{self, threads::lower_thread_priority},
    server::error::{Error, Result},
    settings,
    stream::{gst as gst_stream, manager as stream_manager, stats::pipeline_analysis},
    video::{
        types::VideoSourceTypeExt,
        video_source::{self, VideoSource, VideoSourceFormats},
        xml,
    },
};

pub fn new_info() -> Info {
    Info {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        sha: option_env!("VERGEN_GIT_SHA").unwrap_or("?").into(),
        build_date: env!("VERGEN_BUILD_TIMESTAMP").into(),
        authors: env!("CARGO_PKG_AUTHORS").into(),
        development: Development {
            number_of_tasks: helper::threads::process_task_counter(),
        },
    }
}

use include_dir::{include_dir, Dir};

static DIST: Dir<'_> = include_dir!("frontend/dist");

fn load_file(file_name: &str) -> Option<&'static str> {
    DIST.get_file(file_name)
        .and_then(|file| file.contents_utf8())
}

#[api_v2_operation]
pub fn root(req: HttpRequest) -> Result<HttpResponse> {
    let raw = req.match_info().get("filename").unwrap_or("");
    let filename = if raw.is_empty() { "index.html" } else { raw };

    // Try exact file match
    if let Some(content) = load_file(filename) {
        let extension = Path::new(filename)
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or("");
        let mime = actix_files::file_extension_to_mime(extension).to_string();
        return Ok(HttpResponse::Ok().content_type(mime).body(content));
    }

    // Try directory index: {path}/index.html
    let dir_index = format!("{}/index.html", filename.trim_end_matches('/'));
    if let Some(content) = load_file(&dir_index) {
        let mime = actix_files::file_extension_to_mime("html").to_string();
        return Ok(HttpResponse::Ok().content_type(mime).body(content));
    }

    // SPA fallback: serve index.html for client-side routing
    if let Some(content) = load_file("index.html") {
        let mime = actix_files::file_extension_to_mime("html").to_string();
        return Ok(HttpResponse::Ok().content_type(mime).body(content));
    }

    Err(Error::NotFound(format!(
        "Page does not exist: {filename:?}"
    )))
}

#[api_v2_operation]
/// Provide information about the running service
/// There is no stable API guarantee for the development field
pub async fn info() -> Result<CreatedJson<Info>> {
    Ok(CreatedJson(new_info()))
}

//TODO: change endpoint name to sources
#[api_v2_operation]
/// Provides list of all video sources, with controls and formats
pub async fn v4l() -> Result<Json<Vec<ApiVideoSource>>> {
    let cameras = video_source::cameras_available().await;
    let blocked_sources = stream_manager::blocked_sources();

    use futures::stream::{self, StreamExt};

    let cameras: Vec<ApiVideoSource> = stream::iter(cameras)
        .then(|cam| {
            let blocked_sources = &blocked_sources;
            async move {
                let source_string = cam.inner().source_string();
                let blocked = blocked_sources.iter().any(|s| s == source_string);

                match cam {
                    VideoSourceType::Local(local) => ApiVideoSource {
                        name: local.name().clone(),
                        source: local.source_string().to_string(),
                        formats: local.formats().await,
                        controls: local.controls(),
                        blocked,
                    },
                    VideoSourceType::Gst(gst) => ApiVideoSource {
                        name: gst.name().clone(),
                        source: gst.source_string().to_string(),
                        formats: gst.formats().await,
                        controls: gst.controls(),
                        blocked,
                    },
                    VideoSourceType::Onvif(onvif) => ApiVideoSource {
                        name: onvif.name().clone(),
                        source: onvif.source_string().to_string(),
                        formats: onvif.formats().await,
                        controls: onvif.controls(),
                        blocked,
                    },
                    VideoSourceType::Redirect(redirect) => ApiVideoSource {
                        name: redirect.name().clone(),
                        source: redirect.source_string().to_string(),
                        formats: redirect.formats().await,
                        controls: redirect.controls(),
                        blocked,
                    },
                }
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

        crate::controls::onvif::manager::Manager::reset()
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
/// Blocks a video source and removes all streams using it
pub async fn block_source(query: web::Query<BlockSource>) -> Result<HttpResponse> {
    stream_manager::block_source(&query.source_string)
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    let blocked_sources = stream_manager::blocked_sources();

    let json = serde_json::to_string_pretty(&blocked_sources)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
/// Unblocks a video source
pub async fn unblock_source(query: web::Query<UnblockSource>) -> Result<HttpResponse> {
    stream_manager::unblock_source(&query.source_string)
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    let blocked_sources = stream_manager::blocked_sources();

    let json = serde_json::to_string_pretty(&blocked_sources)
        .map_err(|error| Error::Internal(format!("{error:?}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

#[api_v2_operation]
/// Returns the list of blocked video sources
pub async fn blocked_sources() -> Result<Json<Vec<String>>> {
    let blocked_sources = stream_manager::blocked_sources();
    Ok(Json(blocked_sources))
}

#[api_v2_operation]
/// Clears all blocked video sources
pub async fn clear_blocked_sources() -> Result<HttpResponse> {
    stream_manager::clear_blocked_sources().await;
    Ok(HttpResponse::Ok().finish())
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
    // To workaround this, we are manually validating here.
    // Note: `ThumbnailFileRequest` used to derive `validator::Validate`, but since
    // it moved to the `mcm-api` crate (which doesn't depend on `validator`), we now
    // do the range checks inline instead.
    // TODO: update this function to use `actix_web_validator::Query` directly and get
    // rid of this workaround.
    if let Some(quality) = thumbnail_file_request.quality {
        if !(1..=100).contains(&quality) {
            return Err(Error::Internal(format!(
                "Quality must be between 1 and 100, got {quality}"
            )));
        }
    }
    if let Some(target_height) = thumbnail_file_request.target_height {
        if !(1..=1080).contains(&target_height) {
            return Err(Error::Internal(format!(
                "Target height must be between 1 and 1080, got {target_height}"
            )));
        }
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

#[api_v2_operation]
/// WebSocket endpoint that streams the DOT representation of all running GStreamer pipelines in real time.
/// Requires `--enable-dot` CLI flag; returns 404 when disabled.
pub async fn dot_stream(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse> {
    if !crate::cli::manager::is_dot_enabled() {
        return Err(Error::NotFound(
            "DOT endpoint disabled. Start with --enable-dot to enable.".into(),
        ));
    }

    let (response, mut session, mut msg_stream) =
        actix_ws::handle(&req, stream).map_err(|error| Error::Internal(format!("{error:?}")))?;

    rt::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut last_dots = std::collections::HashMap::new();

        loop {
            tokio::select! {
                Some(msg) = msg_stream.next() => {
                    match msg {
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Text(_)) | Ok(Message::Binary(_)) | Ok(Message::Continuation(_)) | Ok(Message::Nop) => continue,
                        Ok(Message::Close(_)) | Err(_) => break,
                    }
                }
                _ = interval.tick() => {
                    let mut dots = Vec::new();
                    let mut changed = false;

                    match crate::stream::manager::streams().await {
                        Ok(streams) => {
                            for stream_info in streams {
                                if let Some((dot, children)) = crate::stream::manager::Manager::get_stream_dot_by_id(&stream_info.id).await {
                                    let id = stream_info.id.to_string();
                                    if last_dots.get(&id) != Some(&dot) {
                                        last_dots.insert(id.clone(), dot.clone());
                                        changed = true;
                                        dots.push(json!({
                                            "id": id,
                                            "dot": dot,
                                            "children": children,
                                        }));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            warn!("Failed to get streams information: {error:?}");
                        }
                    }

                    if changed {
                        let msg = match serde_json::to_string(&dots) {
                            Ok(msg) => msg,
                            Err(error) => {
                                warn!("Failed to serialize DOT data: {error:?}");
                                "[]".to_string()
                            }
                        };

                        if session.text(msg).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    Ok(response)
}

/// Consolidated snapshot of all streams.
pub async fn streams_snapshot_get(query: web::Query<SnapshotQuery>) -> Result<HttpResponse> {
    let buffer_limit = query.into_inner().buffer_limit;
    let streams_info = stream_manager::streams()
        .await
        .map_err(|error| Error::Internal(format!("{error:?}")))?;
    let json_bytes = tokio::task::spawn_blocking(move || {
        lower_thread_priority();
        pipeline_analysis::full_snapshot_json(buffer_limit, &streams_info)
    })
    .await
    .map_err(|error| Error::Internal(format!("{error:?}")))?;
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json_bytes.as_ref().clone()))
}

/// WebSocket endpoint that streams snapshots at a configurable interval.
pub async fn streams_snapshot_ws(
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<SnapshotWsQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let interval_ms = query.interval_ms.max(500);
    let buffer_limit = query.buffer_limit;
    let (response, mut session, mut msg_stream) =
        actix_ws::handle(&req, stream).map_err(|error| Error::Internal(format!("{error:?}")))?;

    rt::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
        loop {
            tokio::select! {
                Some(msg) = msg_stream.next() => {
                    match msg {
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) |
                        Ok(Message::Text(_)) | Ok(Message::Binary(_)) |
                        Ok(Message::Continuation(_)) | Ok(Message::Nop) => continue,
                        Ok(Message::Close(_)) | Err(_) => break,
                    }
                }
                _ = interval.tick() => {
                    let streams_info = match stream_manager::streams().await {
                        Ok(info) => info,
                        Err(error) => {
                            warn!("Failed to get streams info for WS snapshot: {error:?}");
                            continue;
                        }
                    };
                    let json_bytes = match tokio::task::spawn_blocking(move || {
                        lower_thread_priority();
                        pipeline_analysis::full_snapshot_json(buffer_limit, &streams_info)
                    }).await {
                        Ok(bytes) => bytes,
                        Err(_) => continue,
                    };
                    // Safety: serde_json always produces valid UTF-8.
                    let json_str = unsafe { std::str::from_utf8_unchecked(&json_bytes) };
                    if session.text(json_str).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    Ok(response)
}

#[api_v2_operation]
/// Reset all stream statistics (clears rolling windows and counters).
pub async fn streams_reset() -> Result<HttpResponse> {
    pipeline_analysis::reset_all();
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body("{\"status\":\"reset\"}"))
}

#[api_v2_operation]
/// Get the stats level (lite/full).
pub async fn streams_level_get() -> Result<HttpResponse> {
    let level = pipeline_analysis::global_stats_level();
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(format!("{{\"level\":\"{level}\"}}")))
}

#[api_v2_operation]
/// Set the stats level. Body: {"level": "lite"} or {"level": "full"}.
/// Note: changing the level only affects newly created pipelines. Existing
/// probes keep their original level until the pipeline restarts.
pub async fn streams_level_set(json: web::Json<SetLevelRequest>) -> Result<HttpResponse> {
    use mcm_api::v1::stats::StatsLevel;

    let level = match json.level.as_str() {
        "lite" => StatsLevel::Lite,
        "full" => StatsLevel::Full,
        other => {
            return Err(Error::BadRequest(format!(
                "Invalid level: {other:?}. Must be \"lite\" or \"full\"."
            )));
        }
    };
    pipeline_analysis::set_global_stats_level(level);
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(format!("{{\"level\":\"{}\"}}", json.level)))
}

#[api_v2_operation]
/// Get the current window size.
pub async fn streams_window_size_get() -> Result<HttpResponse> {
    let size = pipeline_analysis::global_window_size();
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::json!({"window_size": size}).to_string()))
}

#[api_v2_operation]
/// Set the window size. Body: {"window_size": 900}.
/// Note: changing the window size only affects newly created pipelines.
/// Existing probes keep their original window size until the pipeline restarts.
pub async fn streams_window_size_set(
    json: web::Json<SetWindowSizeRequest>,
) -> Result<HttpResponse> {
    if !pipeline_analysis::is_valid_window_size(json.window_size) {
        return Err(Error::BadRequest(format!(
            "window_size must be between 1 and {}",
            pipeline_analysis::MAX_WINDOW_SIZE
        )));
    }
    pipeline_analysis::set_global_window_size(json.window_size);
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::json!({"window_size": json.window_size}).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::ResponseError;

    #[actix_web::test]
    async fn window_size_set_rejects_zero() {
        let result =
            streams_window_size_set(web::Json(SetWindowSizeRequest { window_size: 0 })).await;

        let err = result.expect_err("expected bad request");
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
    }
}
