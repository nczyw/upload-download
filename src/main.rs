mod handlers;
mod i18n;
mod password;

use actix_web::{web, App, HttpServer};
use clap::Parser;
use log::{info, warn};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::fs;

use handlers::RateLimiter;

#[derive(Parser, Debug)]
#[command(version, about = "File upload & download server with dynamic password protection")]
struct Args {
    #[arg(short, long, default_value_t = 80)]
    port: u16,

    #[arg(short = 'w', long)]
    password: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    #[serde(rename = "salt")]
    salt_section: SaltSection,
}

#[derive(Debug, Deserialize, Serialize)]
struct SaltSection {
    salt: String,
}

fn generate_random_salt() -> String {
    let charset: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..32).map(|_| charset[rng.gen_range(0..charset.len())] as char).collect()
}

async fn load_or_create_salt() -> String {
    let config_path = "config/config.toml";
    
    if let Err(e) = fs::create_dir_all("config").await {
        warn!("Failed to create config directory: {}", e);
    }
    
    if let Ok(content) = fs::read_to_string(config_path).await {
        match toml::from_str::<Config>(&content) {
            Ok(config) => {
                info!("Loaded salt from config");
                return config.salt_section.salt;
            }
            Err(e) => {
                warn!("Failed to parse config.toml: {}, regenerating", e);
            }
        }
    }
    
    let salt = generate_random_salt();
    let config = Config {
        salt_section: SaltSection { salt: salt.clone() },
    };
    
    let content = toml::to_string(&config).unwrap();
    if let Err(e) = fs::write(config_path, content).await {
        warn!("Failed to write config.toml: {}", e);
    } else {
        info!("Generated new salt and saved to config.toml");
    }
    
    salt
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

    if let Err(e) = fs::create_dir_all("data").await {
        eprintln!("Failed to create data directory: {}", e);
        std::process::exit(1);
    }

    let salt = load_or_create_salt().await;

    info!("Starting server on {}", bind_addr);
    info!("Upload directory: data/");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(handlers::DeletePassword(delete_password.clone())))
            .app_data(web::Data::new(handlers::Salt(salt.clone())))
            .app_data(limiter.clone())
            .app_data(web::PayloadConfig::new(10 * 1024 * 1024 * 1024))
            .route("/", web::get().to(handlers::index))
            .route("/api/files", web::get().to(handlers::list_files))
            .route("/api/upload", web::post().to(handlers::upload))
            .route("/api/download/{filename:.*}", web::get().to(handlers::download))
            .route("/api/delete/{filename:.*}", web::post().to(handlers::delete_file))
            .route("/api/verify", web::post().to(handlers::verify_password))
            .route("/api/lang", web::get().to(handlers::get_translations))
            .route("/api/salt", web::post().to(handlers::get_salt))
    })
    .bind(&bind_addr)?
    .run()
    .await
}
