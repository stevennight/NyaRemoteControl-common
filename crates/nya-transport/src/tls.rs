//! rustls configuration.
//!
//! There is no CA: both sides present self-signed certificates. The client
//! pins the server certificate fingerprint (after pairing); the server accepts
//! any client certificate at the TLS layer and authorises the fingerprint at
//! the application layer (paired list or pairing handshake).

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::identity::{Fingerprint, Identity};

/// Text of the error raised when the server certificate doesn't match the
/// pinned fingerprint (clients match on it to offer re-verification).
pub const PIN_MISMATCH: &str = "被控端证书指纹与已保存的不一致";

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Server side: require a client certificate but accept any; the fingerprint
/// is checked after the handshake.
#[derive(Debug)]
struct AnyClientCert {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for AnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
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

/// Client side: accept the server certificate only if it matches the pinned
/// fingerprint. With no pin (first pairing) any certificate is accepted; the
/// pairing handshake then proves the server knows the pairing code.
#[derive(Debug)]
struct PinnedServerCert {
    provider: Arc<CryptoProvider>,
    pinned: Option<Fingerprint>,
}

impl ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.pinned {
            Some(pin) if Fingerprint::of_der(end_entity.as_ref()) != pin => {
                Err(rustls::Error::General(format!("{PIN_MISMATCH}（可能被控端重装过，或存在中间人）")))
            }
            _ => Ok(ServerCertVerified::assertion()),
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

pub fn server_crypto(id: &Identity) -> Result<rustls::ServerConfig> {
    let provider = provider();
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS versions")?
        .with_client_cert_verifier(Arc::new(AnyClientCert { provider }))
        .with_single_cert(id.cert_chain(), id.private_key())
        .context("server certificate")?;
    cfg.alpn_protocols = vec![nya_proto::ALPN.to_vec()];
    Ok(cfg)
}

pub fn client_crypto(id: &Identity, pinned: Option<Fingerprint>) -> Result<rustls::ClientConfig> {
    let provider = provider();
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS versions")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServerCert { provider, pinned }))
        .with_client_auth_cert(id.cert_chain(), id.private_key())
        .context("client certificate")?;
    cfg.alpn_protocols = vec![nya_proto::ALPN.to_vec()];
    Ok(cfg)
}
