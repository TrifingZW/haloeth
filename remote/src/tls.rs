//! 自签证书生成与指纹计算。
//!
//! 远程服务的 TLS 采用自签证书：客户端通过配对信息中的 SHA-256 指纹钉扎
//! （TOFU），不依赖公共 CA。

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// 自签证书材料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfSigned {
    /// 证书 PEM。
    pub cert_pem: String,
    /// 私钥 PEM。
    pub key_pem: String,
    /// 证书 DER 的 SHA-256 指纹（十六进制），供客户端钉扎。
    pub fingerprint: String,
}

/// 生成自签证书。
///
/// `hosts` 中的 IP 写入 IP SAN，其余写入 DNS SAN；为空时使用 `localhost`。
pub fn self_signed(hosts: &[String]) -> Result<SelfSigned, String> {
    let hosts = if hosts.is_empty() {
        vec!["localhost".to_owned()]
    } else {
        hosts.to_vec()
    };
    let params =
        rcgen::CertificateParams::new(hosts).map_err(|error| format!("证书参数无效：{error}"))?;
    let key = rcgen::KeyPair::generate().map_err(|error| format!("生成密钥失败：{error}"))?;
    let cert = params.self_signed(&key).map_err(|error| format!("签发证书失败：{error}"))?;
    Ok(SelfSigned {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        fingerprint: fingerprint(cert.der()),
    })
}

/// 证书 DER 的 SHA-256 指纹（十六进制）。
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 从 PEM 证书计算指纹；PEM 无法解析时返回 [`None`]。
pub fn fingerprint_pem(cert_pem: &str) -> Option<String> {
    let mut reader = std::io::Cursor::new(cert_pem.as_bytes());
    let cert = rustls_pemfile::certs(&mut reader).next()?.ok()?;
    Some(fingerprint(cert.as_ref()))
}
