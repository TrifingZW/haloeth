//! 设备令牌生成与哈希校验。
//!
//! 桌面端只保存摘要：配对时展示一次原始令牌，之后连接只比较摘要。

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// 生成 256 位随机令牌（64 位十六进制）。
///
/// 以两次 UUID v4 拼接，避免为令牌引入额外的随机数依赖。
pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// 令牌摘要（SHA-256 十六进制）。
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 比较令牌与已存摘要。
pub fn verify(token: &str, hash: &str) -> bool {
    hash_token(token) == hash
}
