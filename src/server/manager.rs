use super::pages;

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
                trace!("{:#?}", &req);
                let fut = srv.call(req);
                async { fut.await }
            })
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
            .route("/info", web::get().to(pages::info))
            .route("/delete_stream", web::delete().to(pages::remove_stream))
            .route("/reset_settings", web::post().to(pages::reset_settings))
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
            .build()
    })
    .bind(server_address)
    .expect("Failed starting web API")
    .run()
    .await
}
