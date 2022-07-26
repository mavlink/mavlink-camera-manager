use super::pages;
use crate::cli;

use crate::stream::webrtc::{signalling_server::SignallingServer, utils::is_webrtcsink_available};

use actix_service::Service;
use actix_web::{error::JsonPayloadError, App, HttpRequest, HttpServer};
use paperclip::{
    actix::{web, OpenApiExt},
    v2::models::{Api, Info},
};

use log::*;

fn json_error_handler(error: JsonPayloadError, _: &HttpRequest) -> actix_web::Error {
    warn!("Problem with json: {error}");
    error.into()
}

// Start REST API server with the desired address
pub async fn run(server_address: &str) -> Result<(), std::io::Error> {
    let webrtc_available = is_webrtcsink_available();
    if webrtc_available {
        // Start WebRTC signalling server before the HTTP so it can answer any request comming from the http front-end.
        SignallingServer::start();
    } else {
        warn!(
            "WebRTC front-end cannot be started because the gstreamer webrtcsink plugin is not available. {}",
            crate::stream::webrtc::utils::webrtcsink_installation_instructions()
        );
    }

    let server_address = server_address.to_string();

    HttpServer::new(move || {
        let mut app = App::new();

        if let Some(path) = cli::manager::www_path() {
            if webrtc_available {
                // Load static files from configured www-path
                info!("Using the www-path: {path:?}.");
                app = app.service(
                    actix_files::Files::new("webrtc", format!("{path}/webrtc/"))
                        .index_file("index.html"),
                );
            }
        };

        // Add debug call for API access
        app.wrap_fn(|req, srv| {
            debug!("{:#?}", &req);
            let fut = srv.call(req);
            async { Ok(fut.await?) }
        })
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
        .build()
    })
    .bind(server_address)
    .unwrap()
    .run()
    .await
}
