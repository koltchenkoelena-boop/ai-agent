use actix_web::{middleware::Logger, web, App, HttpServer};
use actix_session::{SessionMiddleware, storage::CookieSessionStore};
use actix_files::Files;
use dotenv::dotenv;
use std::env;

mod auth;
mod routes;
mod config;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    // Session secret key (should be 32 bytes, base64 encoded)
    let secret_key = env::var("SESSION_SECRET_KEY")
        .unwrap_or_else(|_| {
            // Generate a random key for development (not suitable for production)
            use rand::Rng;
            let mut rng = rand::thread_rng();
            let mut key = [0u8; 32];
            rng.fill(&mut key);
            base64::encode(key)
        });

    let bind_addr = format!("{}:{}", env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string()), env::var("PORT").unwrap_or_else(|_| "8080".to_string()));
    println!("Starting server at http://{}", bind_addr);

    HttpServer::new(move || {
        App::new()
            .wrap(Logger::default())
            .wrap(SessionMiddleware::new(
                CookieSessionStore::default(),
                secret_key.as_bytes(),
            ))
            .service(web::scope("/api")
                .configure(routes::init))
            .service(Files::new("/", "./static").index_file("index.html"))
    })
    .bind(bind_addr)?
    .run()
    .await
}