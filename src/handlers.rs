use actix_multipart::Multipart;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Local};
use futures_util::StreamExt;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone)]
pub struct DeletePassword(pub String);

#[derive(Clone)]
pub struct Salt(pub String);

const HTML_TEMPLATE: &str = include_str!("../static/index.html");
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::i18n;
use crate::password;

const UPLOAD_DIR: &str = "data";

#[derive(Serialize)]
struct FileInfo {
    name: String,
    size: u64,
    modified: String,
}

#[derive(Serialize)]
struct UploadResult {
    original: String,
    saved_as: String,
}

#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    success: bool,
    message: String,
    data: Option<T>,
}

#[derive(Deserialize)]
pub struct PasswordQuery {
    pub password: String,
}

#[derive(Deserialize)]
pub struct PasswordBody {
    pub password: String,
}

fn ok_msg<T: Serialize>(msg: String, data: Option<T>) -> HttpResponse {
    HttpResponse::Ok().json(ApiResponse { success: true, message: msg, data })
}

fn err_msg(msg: String) -> HttpResponse {
    HttpResponse::Ok().json(ApiResponse::<()> { success: false, message: msg, data: None })
}

fn lang_from_req(req: &HttpRequest) -> String {
    if let Some(lang) = req.headers().get("X-Lang") {
        if let Ok(v) = lang.to_str() {
            if v == "zh" || v == "en" {
                return v.to_string();
            }
        }
    }
    let accept = req
        .headers()
        .get("Accept-Language")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("en");
    i18n::detect(accept)
}

fn client_ip(req: &HttpRequest) -> String {
    if let Some(v) = req.headers().get("X-Forwarded-For").and_then(|v| v.to_str().ok()) {
        if let Some(ip) = v.split(',').next() {
            return ip.trim().to_string();
        }
    }
    req.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default()
}

// ============ Rate Limiter ============

pub struct RateLimiter {
    inner: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter { inner: Mutex::new(HashMap::new()) }
    }

    /// Check if the key has exceeded the rate limit (3 attempts per 10 minutes).
    /// Returns Ok if allowed, Err with remaining cooldown seconds if blocked.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let entries = map.entry(key.to_string()).or_default();
        entries.retain(|t| now.duration_since(*t).as_secs() < 600);
        if entries.len() >= 3 {
            let oldest = entries[0];
            let wait = 600 - now.duration_since(oldest).as_secs();
            return Err(wait);
        }
        Ok(())
    }

    pub fn record_failure(&self, key: &str) {
        let mut map = self.inner.lock().unwrap();
        map.entry(key.to_string()).or_default().push(Instant::now());
    }

    pub fn reset(&self, key: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(key);
    }
}

fn find_available_path(dir: &Path, filename: &str) -> PathBuf {
    let filepath = dir.join(filename);
    if !filepath.exists() {
        return filepath;
    }

    let stem = Path::new(filename).file_stem().unwrap_or_default().to_string_lossy();
    let ext = Path::new(filename).extension().unwrap_or_default().to_string_lossy();
    
    for i in 2.. {
        let new_name = if ext.is_empty() {
            format!("{}-{}", stem, i)
        } else {
            format!("{}-{}.{}", stem, i, ext)
        };
        let new_path = dir.join(&new_name);
        if !new_path.exists() {
            return new_path;
        }
    }
    dir.join(filename)
}

// ============ Handlers ============

pub async fn index() -> HttpResponse {
    let html = HTML_TEMPLATE
        .replace("AUTHOR_PLACEHOLDER", env!("CARGO_PKG_AUTHORS"))
        .replace("VERSION_PLACEHOLDER", env!("CARGO_PKG_VERSION"));
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html)
}

pub async fn list_files() -> HttpResponse {
    let dir = Path::new(UPLOAD_DIR);
    if !dir.exists() {
        return ok_msg(String::new(), Some(Vec::<FileInfo>::new()));
    }

    let mut entries = Vec::new();
    let mut read_dir = match fs::read_dir(dir).await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to read upload dir: {}", e);
            return err_msg(e.to_string());
        }
    };

    loop {
        let entry = match read_dir.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => {
                error!("Failed to read dir entry: {}", e);
                continue;
            }
        };
        if entry.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
            let meta = entry.metadata().await.unwrap();
            let modified: DateTime<Local> = meta.modified().unwrap().into();
            entries.push(FileInfo {
                name: entry.file_name().to_string_lossy().to_string(),
                size: meta.len(),
                modified: modified.format("%Y-%m-%d %H:%M:%S").to_string(),
            });
        }
    }

    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    ok_msg(String::new(), Some(entries))
}

pub async fn upload(mut payload: Multipart, req: HttpRequest) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    let dir = Path::new(UPLOAD_DIR);
    if !dir.exists() {
        if let Err(e) = fs::create_dir_all(dir).await {
            error!("Failed to create upload dir: {}", e);
            return err_msg(e.to_string());
        }
    }

    let mut uploaded_files = Vec::new();

    while let Some(field) = payload.next().await {
        let mut field = match field {
            Ok(f) => f,
            Err(e) => {
                error!("Multipart error: {}", e);
                continue;
            }
        };

        let filename = field
            .content_disposition()
            .and_then(|cd| cd.get_filename())
            .unwrap_or("file")
            .to_string()
            .replace('/', "_")
            .replace('\\', "_");

        let filepath = find_available_path(dir, &filename);
        let final_filename = filepath.file_name().unwrap().to_string_lossy().to_string();

        let mut f = match fs::File::create(&filepath).await {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to create file {}: {}", final_filename, e);
                return err_msg(e.to_string());
            }
        };

        while let Some(chunk) = field.next().await {
            match chunk {
                Ok(bytes) => {
                    if let Err(e) = f.write_all(&bytes).await {
                        error!("Failed to write file {}: {}", final_filename, e);
                        return err_msg(e.to_string());
                    }
                }
                Err(e) => {
                    error!("Stream error for {}: {}", final_filename, e);
                    return err_msg(e.to_string());
                }
            }
        }

        uploaded_files.push(UploadResult {
            original: filename,
            saved_as: final_filename.clone(),
        });
        info!("File uploaded: {} ({})", final_filename, filepath.display());
    }

    ok_msg(t.upload_success.to_string(), Some(uploaded_files))
}

pub async fn download(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PasswordQuery>,
    limiter: web::Data<RateLimiter>,
    salt: web::Data<Salt>,
) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    let filename = path.into_inner();
    let filepath = PathBuf::from(UPLOAD_DIR).join(&filename);
    let ip_key = client_ip(&req).to_string();

    if !filepath.exists() {
        return err_msg(format!("File not found: {}", filename));
    }

    if let Err(_) = limiter.check(&ip_key) {
        warn!("Rate limit hit for download: {}", ip_key);
        return err_msg(t.too_many_attempts.to_string());
    }

    if !password::verify(&query.password, &salt.0) {
        limiter.record_failure(&ip_key);
        return err_msg(t.invalid_password.to_string());
    }

    limiter.reset(&ip_key);
    info!("Downloading file: {}", filename);

    match fs::read(&filepath).await {
        Ok(data) => {
            let content_type = mime_guess::from_path(&filename)
                .first_or_octet_stream()
                .to_string();
            HttpResponse::Ok()
                .insert_header(("Content-Type", content_type))
                .insert_header(("Content-Disposition", format!("attachment; filename=\"{}\"", filename)))
                .insert_header(("Content-Length", data.len().to_string()))
                .body(data)
        }
        Err(e) => {
            error!("Failed to read file {}: {}", filename, e);
            return err_msg(e.to_string());
        }
    }
}

pub async fn delete_file(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PasswordBody>,
    delete_password: web::Data<DeletePassword>,
    limiter: web::Data<RateLimiter>,
) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    let filename = path.into_inner();
    let ip_key = client_ip(&req).to_string();

    if let Err(_) = limiter.check(&ip_key) {
        warn!("Rate limit hit for delete: {}", ip_key);
        return err_msg(t.too_many_attempts.to_string());
    }

    if body.password != delete_password.0 {
        limiter.record_failure(&ip_key);
        return err_msg(t.invalid_password.to_string());
    }

    let filepath = PathBuf::from(UPLOAD_DIR).join(&filename);
    if !filepath.exists() {
        return err_msg(format!("File not found: {}", filename));
    }

    match fs::remove_file(&filepath).await {
        Ok(_) => {
            limiter.reset(&ip_key);
            info!("File deleted: {}", filename);
            return ok_msg(t.file_deleted.to_string(), None::<()>);
        }
        Err(e) => {
            error!("Failed to delete file {}: {}", filename, e);
            return err_msg(e.to_string());
        }
    }
}

pub async fn verify_password(
    req: HttpRequest,
    body: web::Json<PasswordBody>,
    limiter: web::Data<RateLimiter>,
    salt: web::Data<Salt>,
) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    let ip_key = client_ip(&req).to_string();

    if let Err(_) = limiter.check(&ip_key) {
        warn!("Rate limit hit for password verify: {}", ip_key);
        return err_msg(t.too_many_attempts.to_string());
    }

    if password::verify(&body.password, &salt.0) {
        limiter.reset(&ip_key);
        return ok_msg(String::new(), None::<()>);
    } else {
        limiter.record_failure(&ip_key);
        return err_msg(t.invalid_password.to_string());
    }
}

pub async fn get_salt(
    req: HttpRequest,
    body: web::Json<PasswordBody>,
    salt: web::Data<Salt>,
    delete_password: web::Data<DeletePassword>,
    limiter: web::Data<RateLimiter>,
) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    let ip_key = client_ip(&req).to_string();

    if let Err(_) = limiter.check(&ip_key) {
        warn!("Rate limit hit for get_salt: {}", ip_key);
        return err_msg(t.too_many_attempts.to_string());
    }

    if body.password != delete_password.0 {
        limiter.record_failure(&ip_key);
        return err_msg(t.invalid_password.to_string());
    }

    limiter.reset(&ip_key);
    ok_msg(String::new(), Some(salt.0.clone()))
}

pub async fn get_translations(req: HttpRequest) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    HttpResponse::Ok().json(t)
}
