use regex::Regex;

mod helper;
mod mavlink_camera_information;

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};

mod settings;

use serde_derive::Deserialize;

use std::sync::{Arc, Mutex};

use notify;

fn main() {
    let settings = Arc::new(Mutex::new(settings::settings::Settings::new(
        "/tmp/potato.toml",
    )));
    let settings_pipelines = Arc::clone(&settings);

    let settings_thread = settings.clone();
    std::thread::spawn(move || loop {
        loop {
            println!(".");
            match settings_thread
                .lock()
                .unwrap()
                .file_channel
                .recv_timeout(std::time::Duration::from_millis(1))
            {
                Ok(notification) => match notification {
                    notify::DebouncedEvent::Write(_) => println!("Settings file updated."),
                    _ => {}
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(x) => println!("Error in file settings update: {:#?}", x),
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    println!("thread spawned.");

    HttpServer::new(move || {
        let settings_get_pipelines = Arc::clone(&settings_pipelines);
        let settings_post_pipelines = Arc::clone(&settings_pipelines);

        App::new()
            .route(
                "/",
                web::get().to(move || {
                    "mavlink-camera-manager home page, WIP"
                }),
            )
            .route(
                "/pipelines",
                web::get().to(move |_: HttpRequest| {
                    let settings = settings_get_pipelines.lock().unwrap();

                    return HttpResponse::Ok()
                        .content_type("application/json")
                        .body(serde_json::to_string_pretty(&(*settings).config).unwrap());
                }),
            )
            .route(
                r"/pipelines/{id:\d+}",
                web::post().to(move |x: HttpRequest, id: web::Path<usize>, json: web::Json<settings::settings::VideoConfiguration>| {
                    let mut settings = settings_post_pipelines.lock().unwrap();
                    let number_of_configurations = settings.config.videos_configuration.len();
                    let id = id .into_inner();

                    if (id > number_of_configurations) {
                        return HttpResponse::NotFound()
                            .content_type("text/plain")
                            .body(format!("Maximum id allowed is {}", number_of_configurations));
                    }

                    if (id == number_of_configurations) {
                        settings.config.videos_configuration.push(json.into_inner());
                    } else {
                        settings.config.videos_configuration[id] = json.into_inner();
                    }

                    settings.save().unwrap_or_else(|error| {
                        println!("Failed to save file: {:#?}", error);
                    });

                    return HttpResponse::Ok()
                        .content_type("application/json")
                        .body(serde_json::to_string_pretty(&(*settings).config).unwrap());
                }),
            )
    })
    .bind("0.0.0.0:8088")
    .unwrap()
    .run()
    .unwrap();
}
