use super::pages;

use actix_web::{rt::System, web, App, HttpServer};

pub fn run(server_address: &str) {
    let server_address = server_address.to_string();
    // Start thread
    let _ = System::new("http-server");
    HttpServer::new(|| {
        App::new()
            .route("/", web::get().to(pages::root))
            .route("/xml", web::post().to(pages::xml))
            .route("/v4l", web::get().to(pages::v4l))
            .route("/v4l", web::post().to(pages::v4l_post))
    })
    .bind(server_address)
    .unwrap()
    .run();
}
