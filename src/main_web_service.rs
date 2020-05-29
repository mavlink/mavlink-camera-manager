use regex::Regex;

mod helper;
mod mavlink_camera_information;

use actix_web::{web, App, HttpRequest, HttpServer};

mod settings;

fn main() {
    HttpServer::new(move || {
        App::new()
            .route(
                "/",
                web::get().to(move || {
                    "mavlink-camera-manager"
                }),
            )
            .route(
                "/pipelines",
                web::get().to(move |x: HttpRequest| {
                    format!("pipeline : {:#?}", x)
                }),
            )
            .route(
                r"/pipelines/{id:\d+}",
                web::post().to(move |x: HttpRequest, id: web::Path<u32>, json: web::Json<settings::settings::VideoConfiguration>| {
                    let videoConfiguration = json.into_inner();
                    println!("pipeline/{} : {:#?} {:#?}", id, x, videoConfiguration);
                    format!("pipeline/{} : {:#?} {:#?}", id, x, videoConfiguration)
                }),
            )
    })
    .bind("0.0.0.0:8088")
    .unwrap()
    .run()
    .unwrap();
}
