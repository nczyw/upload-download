mod handlers;
mod i18n;
mod password;

use actix_web::{web, App, HttpServer};
use clap::Parser;
use log::info;
use tokio::fs;

use handlers::RateLimiter;

#[derive(Parser, Debug)]
#[command(version, about = "File upload & download server with dynamic password protection")]
struct Args {
    /// Listening port
    #[arg(short, long, default_value_t = 80)]
    port: u16,

    /// Password for deleting files (required)
    #[arg(short = 'w', long)]
    password: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args = Args::parse();

    let delete_password = args.password.clone();
    let bind_addr = format!("0.0.0.0:{}", args.port);
    let limiter = web::Data::new(RateLimiter::new());

    info!("Starting server on {}", bind_addr);
    info!("Upload directory: data/");

    // Auto-create data directory on startup
    if let Err(e) = fs::create_dir_all("data").await {
        eprintln!("Failed to create data directory: {}", e);
        std::process::exit(1);
    }

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(delete_password.clone()))
            .app_data(limiter.clone())
            .app_data(web::PayloadConfig::new(10 * 1024 * 1024 * 1024)) // 10GB max
            .route("/", web::get().to(handlers::index))
            .route("/api/files", web::get().to(handlers::list_files))
            .route("/api/upload", web::post().to(handlers::upload))
            .route("/api/download/{filename:.*}", web::get().to(handlers::download))
            .route("/api/delete/{filename:.*}", web::delete().to(handlers::delete_file))
            .route("/api/verify", web::post().to(handlers::verify_password))
            .route("/api/lang", web::get().to(handlers::get_translations))
    })
    .bind(&bind_addr)?
    .run()
    .await
}
