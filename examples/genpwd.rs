use sha2::{Digest, Sha256};
use chrono::Utc;

fn main() {
    let seed = Utc::now().format("%Y%m%d%H").to_string();
    let salt = env!("CARGO_PKG_NAME");
    let charset = "23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(salt.as_bytes());
    let result = hasher.finalize();
    let mut password = String::new();
    for i in 0..8 {
        let idx = (result[i] as usize) % charset.len();
        password.push(charset.chars().nth(idx).unwrap());
    }
    println!("seed={}, salt={}, password={}", seed, salt, password);
}
