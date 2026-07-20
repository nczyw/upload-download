use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};

const CHARSET_CHARS: &[char] = &[
    '2','3','4','5','6','7','8','9',
    'A','B','C','D','E','F','G','H','J','K','L','M','N','P','Q','R','S','T','U','V','W','X','Y','Z',
    'a','b','c','d','e','f','g','h','i','j','k','m','n','o','p','q','r','s','t','u','v','w','x','y','z',
];

pub fn calculate(hour_seed: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hour_seed.as_bytes());
    hasher.update(salt.as_bytes());
    let result = hasher.finalize();

    let mut password = String::new();
    for i in 0..8 {
        let index = (result[i] as usize) % CHARSET_CHARS.len();
        password.push(CHARSET_CHARS[index]);
    }
    password
}

fn hour_seed(offset: i64) -> String {
    (Utc::now() + Duration::hours(offset)).format("%Y%m%d%H").to_string()
}

/// Salt is the crate name (e.g. "upload-download"), automatically read from Cargo.toml
const SALT: &str = env!("CARGO_PKG_NAME");

/// Verify using the crate name as salt (auto-detected)
pub fn verify(input: &str) -> bool {
    for offset in &[0, -1] {
        let seed = hour_seed(*offset);
        if input == calculate(&seed, SALT) {
            return true;
        }
    }
    false
}
