# Upload Download Server

A secure file upload and download server built with Rust and Actix-web, featuring dynamic password protection, rate limiting, and internationalization.

## Features

- **File Upload**: Drag & drop or click to upload files up to 10GB
- **Auto-rename on Conflict**: Prevents file overwrites by appending incrementing suffix
- **Dynamic Password Download**: Downloads require a time-based dynamic password (SHA256 hash of UTC hour + salt)
- **Static Password Delete**: Deletion requires a static admin password set at startup
- **Salt Management**: Random salt generated on first run, stored in `config/config.toml`
- **Rate Limiting**: 3 failed password attempts per IP within 10 minutes, then locked
- **Internationalization**: Auto-detect browser language (Chinese/English), with manual toggle
- **Beautiful UI**: Modern dark theme with gradient backgrounds

## Requirements

- Rust 1.70+
- Cargo

## Installation

```bash
git clone https://github.com/nczyw/upload-download.git
cd upload-download
cargo build --release
```

## Usage

```bash
# Basic usage (default port 80, password required)
cargo run -- --password your-admin-password

# Specify port
cargo run -- --port 8080 --password your-admin-password

# Run release build
./target/release/upload-download --port 80 --password your-admin-password
```

### CLI Options

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--port` | `-p` | 80 | Listening port |
| `--password` | `-w` | **required** | Admin password for deleting files and viewing salt |

## Dynamic Password Generation

The download password is generated based on:
- **Time seed**: Current UTC hour in format `YYYYMMDDHH`
- **Salt**: Randomly generated salt stored in `config/config.toml`
- **Algorithm**: SHA256 hash → first 8 bytes mapped to custom charset

### How to Get the Password

First, get the current salt from the web UI (click "Show Salt" button, enter admin password), then use the following formula:

```rust
use chrono::{Duration, Utc};
use sha2::{Sha256, Digest};

fn calculate_dynamic_password(hour_seed: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hour_seed.as_bytes());
    hasher.update(salt.as_bytes());
    let result = hasher.finalize();
    let charset = "23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut password = String::new();
    for i in 0..8 {
        let index = (result[i] as usize) % charset.len();
        password.push(charset.chars().nth(index).unwrap());
    }
    password
}

// Current hour password
let hour_seed = Utc::now().format("%Y%m%d%H").to_string();
let password = calculate_dynamic_password(&hour_seed, "your-salt-from-config");
```

**Note**: The server accepts passwords for both the current hour and the previous hour to avoid boundary issues.

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Web frontend |
| `/api/files` | GET | List uploaded files |
| `/api/upload` | POST | Upload file (multipart/form-data) |
| `/api/download/{filename}` | GET | Download file (requires `?password=` query parameter) |
| `/api/delete/{filename}` | POST | Delete file (password in JSON body) |
| `/api/verify` | POST | Verify dynamic password (password in JSON body) |
| `/api/salt` | POST | Get current salt (password in JSON body) |
| `/api/lang` | GET | Get translations (use `X-Lang` header) |

### Request Body Format

For endpoints requiring password in body:

```json
{
  "password": "your-password"
}
```

## Security

- **Dynamic Password**: Changes every hour, based on UTC time + per-machine salt
- **Salt Protection**: Salt can only be viewed with admin password
- **Password in Body**: Delete and salt requests use POST with password in body (not URL)
- **Rate Limiting**: 3 failed attempts per IP within 10 minutes for download/delete/salt
- **Input Sanitization**: Filenames are sanitized to prevent path traversal
- **HTTPS Ready**: Configure reverse proxy (e.g., Nginx) for production

## Directory Structure

```
upload-download/
├── Cargo.toml
├── src/
│   ├── main.rs          # CLI parsing, server setup, config loading
│   ├── handlers.rs      # HTTP handlers, embedded frontend
│   ├── password.rs      # Dynamic password generation/verification
│   └── i18n.rs          # Translations (Chinese/English)
├── static/
│   └── index.html       # Frontend HTML/CSS/JS (embedded via include_str!)
├── config/              # Configuration directory (auto-created)
│   └── config.toml      # Salt storage (auto-generated)
├── data/                # Upload directory (auto-created)
└── install_ubuntu_service.sh
    uninstall_ubuntu_service.sh
```

## License

MIT