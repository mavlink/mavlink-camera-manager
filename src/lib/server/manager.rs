use actix_cors::Cors;
use actix_extensible_rate_limit::{
    backend::{memory::InMemoryBackend, SimpleInputFunctionBuilder},
    RateLimiter,
};
use actix_service::Service;
use actix_web::{error::JsonPayloadError, App, HttpRequest, HttpServer};
use paperclip::{
    actix::{web, OpenApiExt},
    v2::models::{Api, Info},
};
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
            // Add debug call for API access
            .wrap_fn(|req, srv| {
                trace!("{req:#?}");
                srv.call(req)
            })
            .wrap(
                Cors::default()
                    .allow_any_origin()
                    .allow_any_method()
                    .allow_any_header()
                    .send_wildcard()
                    .max_age(3600),
            )
            .wrap(TracingLogger::default())
            .wrap(actix_web::middleware::Logger::default())
            .wrap_api_with_spec(Api {
                info: Info {
                    version: format!(
                        "{}-{} ({})",
                        env!("CARGO_PKG_VERSION"),
                        env!("VERGEN_GIT_SHA_SHORT"),
                        env!("VERGEN_BUILD_DATE")
                    ),
                    title: env!("CARGO_PKG_NAME").to_string(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .with_json_spec_at("/docs.json")
            .with_swagger_ui_at("/docs")
            // Record services and routes for paperclip OpenAPI plugin for Actix.
            .app_data(web::JsonConfig::default().error_handler(json_error_handler))
            .route("/", web::get().to(pages::root))
            .route(
                r"/{filename:.*(\.html|\.js|\.css)}",
                web::get().to(pages::root),
            )
            .route("/gst_info", web::get().to(pages::gst_info))
            .route("/info", web::get().to(pages::info))
            .route("/delete_stream", web::delete().to(pages::remove_stream))
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
            .service(
                web::scope("/thumbnail")
                    // Add a rate limitter to prevent flood
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
            )
            .build()
    })
    .bind(server_address)
    .expect("Failed starting web API")
    .run()
    .await
}
