use actix_cors::Cors;
use actix_extensible_rate_limit::{
    backend::{memory::InMemoryBackend, SimpleInputFunctionBuilder},
    RateLimiter,
};
use actix_web::{error::JsonPayloadError, web, App, HttpRequest, HttpServer};
use tracing::*;
use tracing_actix_web::TracingLogger;

use super::pages;

fn json_error_handler(error: JsonPayloadError, _: &HttpRequest) -> actix_web::Error {
    warn!("Problem with json: {error}");
    error.into()
}

// Start REST API server with the desired address
pub async fn run(server_address: &str) -> Result<(), std::io::Error> {
    let server_address = server_address.to_string();

    HttpServer::new(move || {
        App::new()
            .wrap(
                Cors::default()
                    .allow_any_origin()
                    .allow_any_method()
                    .allow_any_header()
                    .send_wildcard()
                    .max_age(3600),
            )
            .wrap(TracingLogger::default())
            .app_data(web::JsonConfig::default().error_handler(json_error_handler))
            // Versioned API routes (canonical)
            .service(web::scope("/v1").configure(configure_api_routes))
            // Backward-compatible flat routes (same handlers, for existing clients)
            .configure(configure_api_routes)
            // Static file serving (catch-all, must be last so API routes match first)
            .route("/", web::get().to(pages::root))
            .route(r"/{filename:.+}", web::get().to(pages::root))
    })
    .bind(server_address)
    .expect("Failed starting web API")
    .run()
    .await
}

/// Register all API routes on a `ServiceConfig`.
/// Used for both the `/v1` scope and the backward-compatible flat routes.
fn configure_api_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/gst_info", web::get().to(pages::gst_info))
        .route("/info", web::get().to(pages::info))
        .route("/delete_stream", web::delete().to(pages::remove_stream))
        .route("/block_source", web::post().to(pages::block_source))
        .route("/unblock_source", web::post().to(pages::unblock_source))
        .route("/blocked_sources", web::get().to(pages::blocked_sources))
        .route(
            "/blocked_sources",
            web::delete().to(pages::clear_blocked_sources),
        )
        .route("/reset_settings", web::post().to(pages::reset_settings))
        .route("/restart_streams", web::post().to(pages::restart_streams))
        .route("/streams", web::get().to(pages::streams))
        .route("/streams", web::post().to(pages::streams_post))
        .route("/v4l", web::get().to(pages::v4l))
        .route("/v4l", web::post().to(pages::v4l_post))
        .route(
            "/camera/reset_controls",
            web::post().to(pages::camera_reset_controls),
        )
        .route("/xml", web::get().to(pages::xml))
        .route("/sdp", web::get().to(pages::sdp))
        .route("/log", web::get().to(pages::log))
        .route("/dot", web::get().to(pages::dot_stream))
        .route(
            "/stats/streams/snapshot",
            web::get().to(pages::streams_snapshot_get),
        )
        .route(
            "/stats/streams/snapshot/ws",
            web::get().to(pages::streams_snapshot_ws),
        )
        .route("/stats/streams/reset", web::post().to(pages::streams_reset))
        .route(
            "/stats/streams/level",
            web::get().to(pages::streams_level_get),
        )
        .route(
            "/stats/streams/level",
            web::post().to(pages::streams_level_set),
        )
        .route(
            "/stats/streams/window-size",
            web::get().to(pages::streams_window_size_get),
        )
        .route(
            "/stats/streams/window-size",
            web::post().to(pages::streams_window_size_set),
        )
        .service(
            web::scope("/thumbnail")
                // Add a rate limiter to prevent flood
                .wrap(
                    RateLimiter::builder(
                        InMemoryBackend::builder().build(),
                        SimpleInputFunctionBuilder::new(std::time::Duration::from_secs(1), 4)
                            .real_ip_key()
                            .build(),
                    )
                    .add_headers()
                    .build(),
                )
                .route("", web::get().to(pages::thumbnail)),
        )
        .route("/onvif/devices", web::get().to(pages::onvif_devices))
        .route(
            "/onvif/authentication",
            web::post().to(pages::authenticate_onvif_device),
        )
        .route(
            "/onvif/authentication",
            web::delete().to(pages::unauthenticate_onvif_device),
        );
}
