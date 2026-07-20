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

// ============ Handlers ============

pub async fn index() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(HTML_TEMPLATE)
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

        let filepath = dir.join(&filename);
        let mut f = match fs::File::create(&filepath).await {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to create file {}: {}", filename, e);
                return err_msg(e.to_string());
            }
        };

        while let Some(chunk) = field.next().await {
            match chunk {
                Ok(bytes) => {
                    if let Err(e) = f.write_all(&bytes).await {
                        error!("Failed to write file {}: {}", filename, e);
                        return err_msg(e.to_string());
                    }
                }
                Err(e) => {
                    error!("Stream error for {}: {}", filename, e);
                    return err_msg(e.to_string());
                }
            }
        }

        info!("File uploaded: {} ({})", filename, filepath.display());
    }

    ok_msg(t.upload_success.to_string(), None::<()>)
}

pub async fn download(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PasswordQuery>,
    limiter: web::Data<RateLimiter>,
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

    if !password::verify(&query.password) {
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
    query: web::Query<PasswordQuery>,
    delete_password: web::Data<String>,
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

    if query.password != **delete_password {
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
) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    let ip_key = client_ip(&req).to_string();

    if let Err(_) = limiter.check(&ip_key) {
        warn!("Rate limit hit for password verify: {}", ip_key);
        return err_msg(t.too_many_attempts.to_string());
    }

    if password::verify(&body.password) {
        limiter.reset(&ip_key);
        return ok_msg(String::new(), None::<()>);
    } else {
        limiter.record_failure(&ip_key);
        return err_msg(t.invalid_password.to_string());
    }
}

pub async fn get_translations(req: HttpRequest) -> HttpResponse {
    let lang = lang_from_req(&req);
    let t = i18n::get(&lang);
    HttpResponse::Ok().json(t)
}

// ============ Embedded HTML ============

const HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>File Upload & Download</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
    background: linear-gradient(135deg, #0f0c29, #302b63, #24243e);
    min-height: 100vh; color: #e0e0e0; padding: 20px;
  }
  .container { max-width: 1000px; margin: 0 auto; }
  header {
    display: flex; justify-content: space-between; align-items: center;
    padding: 20px 0; border-bottom: 1px solid rgba(255,255,255,0.1); margin-bottom: 30px;
  }
  header h1 { font-size: 24px; font-weight: 700; background: linear-gradient(90deg, #667eea, #764ba2); -webkit-background-clip: text; -webkit-text-fill-color: transparent; }
  .header-actions { display: flex; gap: 12px; align-items: center; }
  .lang-btn {
    background: rgba(255,255,255,0.1); border: 1px solid rgba(255,255,255,0.2);
    color: #e0e0e0; padding: 8px 16px; border-radius: 8px; cursor: pointer;
    font-size: 14px; transition: all 0.2s;
  }
  .lang-btn:hover { background: rgba(255,255,255,0.2); }
  .upload-area {
    border: 2px dashed rgba(255,255,255,0.3); border-radius: 16px;
    padding: 40px; text-align: center; transition: all 0.3s;
    background: rgba(255,255,255,0.03); cursor: pointer; margin-bottom: 30px;
  }
  .upload-area:hover, .upload-area.drag-over { border-color: #667eea; background: rgba(102,126,234,0.1); }
  .upload-icon { font-size: 48px; margin-bottom: 12px; opacity: 0.6; }
  .upload-text { font-size: 16px; color: rgba(255,255,255,0.7); }
  .upload-btn {
    display: inline-block; margin-top: 16px;
    background: linear-gradient(90deg, #667eea, #764ba2);
    color: white; border: none; padding: 12px 32px; border-radius: 8px;
    font-size: 16px; cursor: pointer; transition: opacity 0.2s;
  }
  .upload-btn:hover { opacity: 0.9; }
  .upload-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  input[type="file"] { display: none; }
  .files-header {
    display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;
  }
  .files-header h2 { font-size: 18px; font-weight: 600; }
  .file-count { font-size: 14px; color: rgba(255,255,255,0.5); }
  .file-grid { display: grid; gap: 12px; }
  .file-card {
    display: flex; align-items: center; justify-content: space-between;
    background: rgba(255,255,255,0.05); border: 1px solid rgba(255,255,255,0.1);
    border-radius: 12px; padding: 16px 20px; transition: all 0.2s;
  }
  .file-card:hover { background: rgba(255,255,255,0.08); border-color: rgba(255,255,255,0.2); }
  .file-info { display: flex; align-items: center; gap: 16px; flex: 1; min-width: 0; }
  .file-icon { font-size: 28px; flex-shrink: 0; }
  .file-details { min-width: 0; }
  .file-name { font-size: 15px; font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .file-meta { font-size: 12px; color: rgba(255,255,255,0.4); margin-top: 4px; display: flex; align-items: center; gap: 8px; }
  .file-actions { display: flex; gap: 8px; flex-shrink: 0; }
  .action-btn {
    padding: 8px 16px; border: none; border-radius: 8px; cursor: pointer;
    font-size: 13px; font-weight: 500; transition: all 0.2s;
  }
  .download-btn { background: rgba(102,126,234,0.2); color: #667eea; }
  .download-btn:hover { background: rgba(102,126,234,0.4); }
  .delete-btn { background: rgba(239,68,68,0.2); color: #ef4444; }
  .delete-btn:hover { background: rgba(239,68,68,0.4); }
  .empty-state { text-align: center; padding: 60px 20px; color: rgba(255,255,255,0.3); }
  .empty-icon { font-size: 64px; margin-bottom: 16px; }
  .modal-overlay {
    display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.6);
    backdrop-filter: blur(4px); align-items: center; justify-content: center; z-index: 1000;
  }
  .modal-overlay.active { display: flex; }
  .modal {
    background: #1e1e2e; border: 1px solid rgba(255,255,255,0.1);
    border-radius: 16px; padding: 32px; width: 400px; max-width: 90vw;
    box-shadow: 0 20px 60px rgba(0,0,0,0.5);
  }
  .modal h3 { font-size: 18px; margin-bottom: 8px; }
  .modal p { font-size: 14px; color: rgba(255,255,255,0.5); margin-bottom: 20px; }
  .modal p.modal-error { color: #ef4444; font-size: 13px; margin-bottom: 12px; display: none; }
  p.modal-error.show { display: block; }
  .modal-input {
    width: 100%; padding: 12px 16px; border: 1px solid rgba(255,255,255,0.2);
    border-radius: 8px; background: rgba(255,255,255,0.05); color: #e0e0e0;
    font-size: 16px; outline: none; transition: border-color 0.2s;
  }
  .modal-input:focus { border-color: #667eea; }
  .modal-actions { display: flex; gap: 12px; justify-content: flex-end; margin-top: 20px; }
  .modal-btn {
    padding: 10px 24px; border: none; border-radius: 8px; cursor: pointer;
    font-size: 14px; font-weight: 500; transition: opacity 0.2s;
  }
  .modal-btn:hover { opacity: 0.9; }
  .modal-btn-primary { background: linear-gradient(90deg, #667eea, #764ba2); color: white; }
  .modal-btn-secondary { background: rgba(255,255,255,0.1); color: #e0e0e0; }
  .modal-btn-danger { background: #ef4444; color: white; }
  .toast {
    position: fixed; bottom: 30px; right: 30px; padding: 16px 24px;
    border-radius: 12px; color: white; font-size: 14px; font-weight: 500;
    opacity: 0; transform: translateY(20px); transition: all 0.3s; z-index: 2000;
  }
  .toast.show { opacity: 1; transform: translateY(0); }
  .toast-success { background: #22c55e; }
  .toast-error { background: #ef4444; }
  .progress-bar {
    width: 100%; height: 4px; background: rgba(255,255,255,0.1);
    border-radius: 2px; margin-top: 16px; overflow: hidden; display: none;
  }
  .progress-bar.active { display: block; }
  .progress-fill {
    height: 100%; background: linear-gradient(90deg, #667eea, #764ba2);
    border-radius: 2px; width: 0%; transition: width 0.3s;
  }
  @media (max-width: 640px) {
    .file-card { flex-direction: column; align-items: stretch; gap: 12px; }
    .file-actions { justify-content: flex-end; }
    header { flex-direction: column; gap: 12px; }
  }
</style>
</head>
<body>
<div class="container">
  <header>
    <h1 id="appTitle">File Upload & Download</h1>
    <div class="header-actions">
      <button class="lang-btn" id="langBtn" onclick="toggleLang()">中文</button>
    </div>
  </header>

  <div class="upload-area" id="uploadArea" onclick="document.getElementById('fileInput').click()">
    <div class="upload-icon">&#128193;</div>
    <div class="upload-text" id="dragText">Drag & drop files here, or click to select</div>
    <button class="upload-btn" id="uploadBtn">Select File</button>
    <input type="file" id="fileInput" multiple onchange="uploadFiles(this.files)">
    <div class="progress-bar" id="progressBar"><div class="progress-fill" id="progressFill"></div></div>
  </div>

  <div class="files-header">
    <h2 id="filesTitle">Files</h2>
    <span class="file-count" id="fileCount"></span>
  </div>

  <div class="file-grid" id="fileGrid">
    <div class="empty-state" id="emptyState">
      <div class="empty-icon">&#128451;</div>
      <p id="emptyText">No files</p>
    </div>
  </div>
</div>

<div class="modal-overlay" id="modalOverlay" onclick="if(event.target===this)closeModal()">
  <div class="modal">
    <h3 id="modalTitle">Download File</h3>
    <p id="modalDesc">Enter dynamic password</p>
    <p class="modal-error" id="modalError"></p>
    <input type="password" class="modal-input" id="modalInput" placeholder="Enter password" autocomplete="off" onkeydown="if(event.key==='Enter')modalConfirm();if(event.key==='Escape')closeModal();">
    <div class="modal-actions">
      <button class="modal-btn modal-btn-secondary" id="modalCancel" onclick="closeModal()">Cancel</button>
      <button class="modal-btn modal-btn-primary" id="modalConfirm" onclick="modalConfirm()" onkeydown="if(event.key==='Escape')closeModal()">Confirm</button>
    </div>
  </div>
</div>

<div class="toast" id="toast"></div>

<script>
let LANG = 'en';
let T = {};
let _fetchId = 0;
let currentAction = null;
let currentFile = null;

function detectLang() {
  const navLang = (navigator.language || navigator.userLanguage || '').toLowerCase();
  if (navLang.startsWith('zh')) return 'zh';
  return 'en';
}

function toggleLang() {
  LANG = LANG === 'en' ? 'zh' : 'en';
  fetchTranslations();
}

async function fetchTranslations() {
  const id = ++_fetchId;
  try {
    const r = await fetch('/api/lang', { headers: { 'X-Lang': LANG } });
    if (!r.ok) { console.error('lang fetch failed:', r.status); return; }
    const data = await r.json();
    if (id !== _fetchId) return;
    T = data;
    updateUI();
  } catch(e) { console.error(e); }
}

function updateUI() {
  document.documentElement.lang = LANG;
  const setText = (id, val) => { const el = document.getElementById(id); if (el) el.textContent = val; };
  setText('appTitle', T.app_title);
  setText('dragText', T.drag_drop);
  setText('uploadBtn', T.select_file);
  setText('filesTitle', T.files);
  setText('modalCancel', T.cancel);
  setText('modalConfirm', T.confirm);
  setText('langBtn', T.language);
  setText('emptyText', T.no_files);
  document.title = T.app_title || '';
  updateFileList();
}

function showToast(msg, type) {
  const toast = document.getElementById('toast');
  toast.textContent = msg;
  toast.className = 'toast toast-' + type + ' show';
  setTimeout(() => toast.classList.remove('show'), 3000);
}

function openModal(action, file) {
  currentAction = action;
  currentFile = file;
  const overlay = document.getElementById('modalOverlay');
  const title = document.getElementById('modalTitle');
  const desc = document.getElementById('modalDesc');
  const input = document.getElementById('modalInput');
  const confirmBtn = document.getElementById('modalConfirm');
  const errEl = document.getElementById('modalError');

  errEl.classList.remove('show');
  errEl.textContent = '';

  if (action === 'download') {
    title.textContent = T.download_title;
    desc.textContent = T.dynamic_password_desc + ': ' + file;
    confirmBtn.textContent = T.download;
    confirmBtn.className = 'modal-btn modal-btn-primary';
    input.placeholder = T.enter_password;
  } else {
    title.textContent = T.delete_title;
    desc.textContent = T.delete_password + ': ' + file;
    confirmBtn.textContent = T.delete_confirm;
    confirmBtn.className = 'modal-btn modal-btn-danger';
    input.placeholder = T.enter_password;
  }
  input.value = '';
  overlay.classList.add('active');
  setTimeout(() => input.focus(), 100);
}

function closeModal() {
  document.getElementById('modalOverlay').classList.remove('active');
  currentAction = null;
  currentFile = null;
}

async function modalConfirm() {
  const password = document.getElementById('modalInput').value;
  const errEl = document.getElementById('modalError');
  if (!password) return;

  if (currentAction === 'download') {
    try {
      const r = await fetch('/api/verify', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Lang': LANG },
        body: JSON.stringify({ password })
      });
      const data = await r.json();
      if (data.success) {
        const file = currentFile;
        closeModal();
        const url = '/api/download/' + encodeURIComponent(file) + '?password=' + encodeURIComponent(password);
        const a = document.createElement('a');
        a.href = url;
        a.download = file;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
      } else {
        errEl.textContent = data.message || T.invalid_password;
        errEl.classList.add('show');
        document.getElementById('modalInput').value = '';
        document.getElementById('modalInput').focus();
      }
    } catch(e) {
      showToast(T.error, 'error');
      closeModal();
    }
  } else if (currentAction === 'delete') {
    try {
      const r = await fetch('/api/delete/' + encodeURIComponent(currentFile) + '?password=' + encodeURIComponent(password), {
        method: 'DELETE',
        headers: { 'X-Lang': LANG }
      });
      const data = await r.json();
      if (data.success) {
        closeModal();
        showToast(data.message, 'success');
        updateFileList();
      } else {
        errEl.textContent = data.message || T.invalid_password;
        errEl.classList.add('show');
        document.getElementById('modalInput').value = '';
        document.getElementById('modalInput').focus();
      }
    } catch(e) {
      showToast(T.error, 'error');
      closeModal();
    }
  }
}

async function uploadFiles(files) {
  if (!files.length) return;
  const formData = new FormData();
  for (const f of files) formData.append('file', f);

  const bar = document.getElementById('progressBar');
  const fill = document.getElementById('progressFill');
  bar.classList.add('active');
  fill.style.width = '0%';

  try {
    let prog = 0;
    const interval = setInterval(() => {
      prog = Math.min(prog + Math.random() * 30, 90);
      fill.style.width = prog + '%';
    }, 300);

    const r = await fetch('/api/upload', {
      method: 'POST',
      body: formData,
      headers: { 'X-Lang': LANG }
    });
    clearInterval(interval);
    const data = await r.json();
    if (data.success) {
      fill.style.width = '100%';
      setTimeout(() => bar.classList.remove('active'), 500);
      showToast(data.message, 'success');
      updateFileList();
    } else {
      bar.classList.remove('active');
      showToast(data.message, 'error');
    }
  } catch(e) {
    bar.classList.remove('active');
    showToast(e.message || T.error, 'error');
  }
}

function formatSize(bytes) {
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  const k = 1024;
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + units[i];
}

function getFileIcon(name) {
  const ext = name.split('.').pop().toLowerCase();
  const icons = {
    pdf: '\u{1F4C4}', doc: '\u{1F4C4}', docx: '\u{1F4C4}',
    xls: '\u{1F4C8}', xlsx: '\u{1F4C8}',
    ppt: '\u{1F4CA}', pptx: '\u{1F4CA}',
    jpg: '\u{1F4F7}', jpeg: '\u{1F4F7}', png: '\u{1F4F7}', gif: '\u{1F4F7}', svg: '\u{1F4F7}', webp: '\u{1F4F7}',
    mp3: '\u{1F3B5}', wav: '\u{1F3B5}', flac: '\u{1F3B5}',
    mp4: '\u{1F3AC}', avi: '\u{1F3AC}', mkv: '\u{1F3AC}', mov: '\u{1F3AC}',
    zip: '\u{1F4E6}', rar: '\u{1F4E6}', '7z': '\u{1F4E6}', tar: '\u{1F4E6}', gz: '\u{1F4E6}',
    txt: '\u{1F4DD}', json: '\u{1F4DD}', xml: '\u{1F4DD}', yaml: '\u{1F4DD}', csv: '\u{1F4DD}',
    rs: '\u{1F4BB}', py: '\u{1F4BB}', js: '\u{1F4BB}', ts: '\u{1F4BB}', go: '\u{1F4BB}',
    html: '\u{1F4BB}', css: '\u{1F4BB}',
  };
  return icons[ext] || '\u{1F4C4}';
}

async function updateFileList() {
  try {
    const r = await fetch('/api/files', { headers: { 'X-Lang': LANG } });
    const data = await r.json();
    const grid = document.getElementById('fileGrid');
    const count = document.getElementById('fileCount');

    if (!data.success || !data.data || data.data.length === 0) {
      grid.innerHTML = '<div class="empty-state"><div class="empty-icon">\u{1F4C4}</div><p>' + (T.no_files || 'No files') + '</p></div>';
      count.textContent = '';
      return;
    }

    count.textContent = (T.total_files || '{n} file(s)').replace('{n}', data.data.length);

    grid.innerHTML = data.data.map(f => {
      const icon = getFileIcon(f.name);
      return '<div class="file-card">' +
        '<div class="file-info">' +
          '<div class="file-icon">' + icon + '</div>' +
          '<div class="file-details">' +
            '<div class="file-name" title="' + escHtml(f.name) + '">' + escHtml(f.name) + '</div>' +
            '<div class="file-meta">' + formatSize(f.size) + ' <span class="size-badge">' + f.modified + '</span></div>' +
          '</div>' +
        '</div>' +
        '<div class="file-actions">' +
          '<button class="action-btn download-btn" onclick="openModal(\'download\',\'' + escAttr(f.name) + '\')">' + T.download + '</button>' +
          '<button class="action-btn delete-btn" onclick="openModal(\'delete\',\'' + escAttr(f.name) + '\')">' + T.delete + '</button>' +
        '</div>' +
      '</div>';
    }).join('');
  } catch(e) { console.error(e); }
}

function escHtml(s) { return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
function escAttr(s) { return s.replace(/&/g,'&amp;').replace(/"/g,'&quot;').replace(/'/g,'&#39;'); }

// Drag and drop
const uploadArea = document.getElementById('uploadArea');
uploadArea.addEventListener('dragover', (e) => { e.preventDefault(); uploadArea.classList.add('drag-over'); });
uploadArea.addEventListener('dragleave', () => { uploadArea.classList.remove('drag-over'); });
uploadArea.addEventListener('drop', (e) => { e.preventDefault(); uploadArea.classList.remove('drag-over'); uploadFiles(e.dataTransfer.files); });

// Init
LANG = detectLang();
fetchTranslations();
</script>
</body>
</html>"#;
