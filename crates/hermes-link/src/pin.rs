//! Trusting one certificate: the one whose fingerprint came with the pairing link.
//!
//! The plugin's certificate is self-signed, so no authority vouches for it. What makes it
//! Hermes's is that the owner carried its SHA-256 to the handheld by hand. A certificate
//! with another fingerprint is refused whatever it says, and the handshake's signature is
//! still checked, so the other end must hold that certificate's key.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    /// Set when a certificate other than the pinned one was shown.
    refused: Arc<AtomicBool>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let seen: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        // Both are public values: nothing is learnt from how long the comparison takes.
        if seen == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            self.refused.store(true, Ordering::Relaxed);
            Err(Error::General(
                "not the certificate this handheld was paired with".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A TLS client that accepts only the certificate with this SHA-256 (64 hex digits), and
/// sets `refused` when another one answers.
pub(crate) fn client_config(
    fingerprint: &str,
    refused: Arc<AtomicBool>,
) -> Result<Arc<ClientConfig>, ()> {
    let bytes = fingerprint.as_bytes();
    if bytes.len() != 64 {
        return Err(());
    }
    let mut pinned = [0u8; 32];
    for (out, pair) in pinned.iter_mut().zip(bytes.chunks_exact(2)) {
        let digits = std::str::from_utf8(pair).map_err(|_| ())?;
        *out = u8::from_str_radix(digits, 16).map_err(|_| ())?;
    }
    let provider = Arc::new(ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| ())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint: pinned,
            refused,
            provider,
        }))
        .with_no_client_auth();
    Ok(Arc::new(config))
}
