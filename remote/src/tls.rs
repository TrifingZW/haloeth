//! 自签证书生成与指纹计算。
//!
//! 远程服务的 TLS 采用自签证书：客户端通过配对信息中的 SHA-256 指纹钉扎
//! （TOFU），不依赖公共 CA。

use std::fmt::Write as _;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
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

/// 构建按指纹钉扎的客户端 TLS 配置。
///
/// 服务器证书的 SHA-256 指纹必须与配对信息一致；主机名不参与校验，
/// 与 TOFU 钉扎的语义一致。
pub fn pinned_client_config(fingerprint: &str) -> Result<rustls::ClientConfig, String> {
    let expected = fingerprint.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("证书指纹必须是 64 位十六进制".to_owned());
    }
    Ok(rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
            expected,
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        }))
        .with_no_client_auth())
}

/// 只比较证书指纹的校验器。
#[derive(Debug)]
struct PinnedVerifier {
    expected: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = fingerprint(end_entity.as_ref());
        if actual.eq_ignore_ascii_case(&self.expected) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("证书指纹与配对信息不一致".to_owned()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
