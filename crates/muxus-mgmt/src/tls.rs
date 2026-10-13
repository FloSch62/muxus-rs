//! TLS for gNMI, done by Muxus rather than the gRPC layer so the certificate
//! can be pinned like an RDP certificate or an SSH host key, and so the same
//! code runs over a direct socket or an SSH gateway channel. gRPC needs
//! HTTP/2, negotiated with ALPN.
//!
//! The verdict on the certificate is Muxus's: a chain that does not verify
//! reaches the user (or is waved through for skip-verify) instead of failing
//! the handshake. Signatures in the handshake itself are always checked.

use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};

use muxus_core::protocol::{DesktopCertificateChallenge, DesktopIdentity, GnmiProfile, TrustState};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::error::Error;

/// A certificate a device presented, as the UI shows and pins it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedCertificate {
    /// Leaf first, as DER.
    pub chain: Vec<Vec<u8>>,
    /// Colon-separated SHA-256 of the leaf, the fingerprint that is pinned.
    pub fingerprint: String,
    pub subject: String,
    pub issuer: String,
    pub valid_from: String,
    pub valid_to: String,
    /// Set when the chain or the host name does not verify.
    pub verification_error: Option<String>,
}

/// The PEM files a gNMI profile names, read.
#[derive(Debug, Clone, Default)]
pub struct TlsFiles {
    pub ca: Option<Vec<u8>>,
    pub cert: Option<Vec<u8>>,
    pub key: Option<Vec<u8>>,
}

async fn read_pem(file: Option<&str>, what: &str) -> Result<Option<Vec<u8>>, Error> {
    let Some(file) = file else {
        return Ok(None);
    };
    tokio::fs::read(file)
        .await
        .map(Some)
        .map_err(|err| Error::other(format!("Could not read the {what} {file}: {err}")))
}

impl TlsFiles {
    /// Read the CA bundle and client certificate a profile names.
    pub async fn read(profile: &GnmiProfile) -> Result<Self, Error> {
        Ok(Self {
            ca: read_pem(profile.ca_file.as_deref(), "CA certificate file").await?,
            cert: read_pem(profile.cert_file.as_deref(), "client certificate file").await?,
            key: read_pem(profile.key_file.as_deref(), "client key file").await?,
        })
    }
}

/// The TLS session over a device stream.
pub struct GnmiTls<IO> {
    pub stream: TlsStream<IO>,
    pub certificate: Option<PresentedCertificate>,
    /// `TLSv1.3`, as Node names it.
    pub protocol: Option<String>,
}

/// SNI must be a DNS name; servers reached by address get none.
pub fn server_name(host: &str) -> Option<String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        None
    } else {
        Some(bare.to_owned())
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Bundled Mozilla roots plus the operating system's store.
static TRUSTED_AUTHORITIES: LazyLock<Arc<RootCertStore>> = LazyLock::new(|| {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // No readable system store: the bundled roots still apply.
    for certificate in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(certificate);
    }
    Arc::new(roots)
});

/// Accepts any certificate, remembering whether it would have verified.
#[derive(Debug)]
struct RecordingVerifier {
    inner: Arc<WebPkiServerVerifier>,
    algorithms: WebPkiSupportedAlgorithms,
    verdict: Mutex<Option<Result<(), rustls::Error>>>,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verdict = self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .map(|_| ());
        *self.verdict.lock().expect("verdict lock") = Some(verdict);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

const NO_H2: &str =
    "The device did not offer HTTP/2 over TLS (ALPN h2), which gNMI needs. Is this a gRPC port?";

fn tls_failure(err: &std::io::Error) -> Error {
    let message = err.to_string();
    let lower = message.to_lowercase();
    if lower.contains("invalidcontenttype")
        || lower.contains("wrong version number")
        || lower.contains("packet length too long")
        || lower.contains("unknown protocol")
        || lower.contains("corrupt message")
    {
        return Error::other(
            "The device did not answer with TLS. If it serves gNMI without encryption, set the host to plain text.",
        );
    }
    if lower.contains("noapplicationprotocol") || lower.contains("no application protocol") {
        return Error::other(NO_H2);
    }
    if lower.contains("certificaterequired") || lower.contains("certificate required") {
        return Error::other(
            "The device requires a client certificate (mutual TLS). Add one in the host settings.",
        );
    }
    Error::other(format!("TLS handshake failed: {message}"))
}

fn roots_from_pem(pem: &[u8]) -> Result<Arc<RootCertStore>, Error> {
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(pem) {
        let certificate = certificate.map_err(|err| {
            Error::other(format!("The CA certificate file is not valid PEM: {err}"))
        })?;
        roots.add(certificate).map_err(|err| {
            Error::other(format!(
                "The CA certificate file holds a bad certificate: {err}"
            ))
        })?;
    }
    Ok(Arc::new(roots))
}

/// Start TLS on a device stream, offering HTTP/2.
pub async fn upgrade_gnmi_tls<IO>(
    stream: IO,
    profile: &GnmiProfile,
    files: &TlsFiles,
) -> Result<GnmiTls<IO>, Error>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let name = profile
        .tls_server_name
        .clone()
        .unwrap_or_else(|| profile.host.clone());
    let provider = provider();
    let roots = match &files.ca {
        Some(pem) => roots_from_pem(pem)?,
        None => TRUSTED_AUTHORITIES.clone(),
    };
    let inner = WebPkiServerVerifier::builder_with_provider(roots, provider.clone())
        .build()
        .map_err(|err| Error::other(format!("TLS setup failed: {err}")))?;
    let verifier = Arc::new(RecordingVerifier {
        inner,
        algorithms: provider.signature_verification_algorithms,
        verdict: Mutex::new(None),
    });
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| Error::other(format!("TLS setup failed: {err}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone());
    let mut config = match (&files.cert, &files.key) {
        (Some(cert), Some(key)) => {
            let chain = CertificateDer::pem_slice_iter(cert)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| {
                    Error::other(format!(
                        "The client certificate file is not valid PEM: {err}"
                    ))
                })?;
            let key = PrivateKeyDer::from_pem_slice(key).map_err(|err| {
                Error::other(format!("The client key file is not valid PEM: {err}"))
            })?;
            builder.with_client_auth_cert(chain, key).map_err(|err| {
                Error::other(format!("The client certificate cannot be used: {err}"))
            })?
        }
        _ => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"h2".to_vec()];
    let target = match server_name(&name) {
        Some(dns) => ServerName::try_from(dns)
            .map_err(|_| Error::other(format!("{name} is not a valid server name.")))?,
        None => ServerName::IpAddress(
            name.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .expect("checked by server_name")
                .into(),
        ),
    };
    let stream = TlsConnector::from(Arc::new(config))
        .connect(target, stream)
        .await
        .map_err(|err| tls_failure(&err))?;
    let (_, session) = stream.get_ref();
    if session.alpn_protocol() != Some(b"h2".as_slice()) {
        return Err(Error::other(NO_H2));
    }
    let protocol = session.protocol_version().map(|version| match version {
        rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_owned(),
        rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_owned(),
        other => format!("{other:?}"),
    });
    let chain: Vec<Vec<u8>> = session
        .peer_certificates()
        .unwrap_or_default()
        .iter()
        .map(|certificate| certificate.as_ref().to_vec())
        .collect();
    let verdict = verifier.verdict.lock().expect("verdict lock").take();
    let certificate = presented_certificate(&chain, &name, verdict);
    Ok(GnmiTls {
        stream,
        certificate,
        protocol,
    })
}

/// `AB:CD:…`, uppercase, like Node's `fingerprint256`.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn distinguished_name(name: &x509_parser::x509::X509Name<'_>) -> String {
    let registry = x509_parser::objects::oid_registry();
    name.iter()
        .map(|rdn| {
            let mut parts: Vec<(String, String)> = Vec::new();
            for attribute in rdn.iter() {
                let key = x509_parser::objects::oid2abbrev(attribute.attr_type(), registry)
                    .map(str::to_owned)
                    .unwrap_or_else(|_| attribute.attr_type().to_id_string());
                let value = attribute.as_str().map(str::to_owned).unwrap_or_else(|_| {
                    String::from_utf8_lossy(attribute.attr_value().data).into_owned()
                });
                parts.push((key, value));
            }
            parts
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// OpenSSL's date form, which Node reports: `Mar  5 12:00:00 2026 GMT`.
fn openssl_time(time: x509_parser::time::ASN1Time) -> String {
    chrono::DateTime::from_timestamp(time.timestamp(), 0)
        .map(|at| at.format("%b %e %H:%M:%S %Y GMT").to_string())
        .unwrap_or_default()
}

fn alt_names(certificate: &x509_parser::certificate::X509Certificate<'_>) -> String {
    use x509_parser::extensions::GeneralName;
    let Ok(Some(san)) = certificate.subject_alternative_name() else {
        return String::new();
    };
    san.value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some(format!("DNS:{dns}")),
            GeneralName::IPAddress(bytes) => match bytes.len() {
                4 => Some(format!(
                    "IP Address:{}",
                    std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])
                )),
                16 => <[u8; 16]>::try_from(*bytes)
                    .ok()
                    .map(|octets| format!("IP Address:{}", std::net::Ipv6Addr::from(octets))),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why a certificate does not verify, worded the way Node words it.
fn verification_message(
    error: &rustls::Error,
    certificate: &x509_parser::certificate::X509Certificate<'_>,
    chain_len: usize,
    host: &str,
) -> String {
    let rustls::Error::InvalidCertificate(error) = error else {
        return error.to_string();
    };
    match error {
        CertificateError::UnknownIssuer => {
            if certificate.subject().as_raw() == certificate.issuer().as_raw() {
                "DEPTH_ZERO_SELF_SIGNED_CERT".into()
            } else if chain_len > 1 {
                "SELF_SIGNED_CERT_IN_CHAIN".into()
            } else {
                "UNABLE_TO_GET_ISSUER_CERT_LOCALLY".into()
            }
        }
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
            "CERT_HAS_EXPIRED".into()
        }
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            "CERT_NOT_YET_VALID".into()
        }
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            if bare.parse::<IpAddr>().is_ok() {
                format!(
                    "Hostname/IP does not match certificate's altnames: IP: {bare} is not in the cert's list: {}",
                    alt_names(certificate)
                )
            } else {
                format!(
                    "Hostname/IP does not match certificate's altnames: Host: {bare}. is not in the cert's altnames: {}",
                    alt_names(certificate)
                )
            }
        }
        CertificateError::BadSignature => "CERT_SIGNATURE_FAILURE".into(),
        CertificateError::Revoked => "CERT_REVOKED".into(),
        other => format!("{other:?}"),
    }
}

/// The leaf's details and whether it verified against trusted authorities.
pub fn presented_certificate(
    chain: &[Vec<u8>],
    host: &str,
    verdict: Option<Result<(), rustls::Error>>,
) -> Option<PresentedCertificate> {
    let leaf = chain.first()?;
    let (_, certificate) = x509_parser::parse_x509_certificate(leaf).ok()?;
    let verification_error = match verdict {
        Some(Ok(())) => None,
        Some(Err(error)) => Some(verification_message(
            &error,
            &certificate,
            chain.len(),
            host,
        )),
        None => Some("The certificate is not trusted.".into()),
    };
    Some(PresentedCertificate {
        chain: chain.to_vec(),
        fingerprint: fingerprint(leaf),
        subject: distinguished_name(certificate.subject()),
        issuer: distinguished_name(certificate.issuer()),
        valid_from: openssl_time(certificate.validity().not_before),
        valid_to: openssl_time(certificate.validity().not_after),
        verification_error,
    })
}

/// What the user must decide about a certificate, or `None` when it can be
/// used without asking: it verifies, or it is the one trusted before.
pub fn certificate_challenge(
    certificate: &PresentedCertificate,
    host: &str,
    port: u16,
    pinned: Option<&str>,
) -> Option<DesktopCertificateChallenge> {
    let error = certificate.verification_error.as_ref()?;
    if pinned == Some(certificate.fingerprint.as_str()) {
        return None;
    }
    Some(DesktopCertificateChallenge {
        host: host.to_owned(),
        port,
        fingerprint: certificate.fingerprint.clone(),
        state: if pinned.is_some() {
            TrustState::Mismatch
        } else {
            TrustState::New
        },
        previous: pinned.map(str::to_owned),
        identity: DesktopIdentity::Certificate {
            subject: certificate.subject.clone(),
            issuer: certificate.issuer.clone(),
            valid_from: certificate.valid_from.clone(),
            valid_to: certificate.valid_to.clone(),
            verification_error: Some(error.clone()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_sni_only_for_names() {
        assert_eq!(server_name("leaf1").as_deref(), Some("leaf1"));
        assert_eq!(server_name("10.0.0.1"), None);
        assert_eq!(server_name("[fd00::1]"), None);
    }

    #[test]
    fn challenges_only_certificates_that_neither_verify_nor_are_pinned() {
        let certificate = PresentedCertificate {
            chain: vec![],
            fingerprint: "AA:BB".into(),
            subject: "CN=leaf1".into(),
            issuer: "CN=leaf1".into(),
            valid_from: "Jan  1 00:00:00 2026 GMT".into(),
            valid_to: "Jan  1 00:00:00 2027 GMT".into(),
            verification_error: Some("DEPTH_ZERO_SELF_SIGNED_CERT".into()),
        };
        let challenge = certificate_challenge(&certificate, "leaf1", 57400, None).unwrap();
        assert_eq!(challenge.state, TrustState::New);
        assert!(certificate_challenge(&certificate, "leaf1", 57400, Some("AA:BB")).is_none());
        let changed = certificate_challenge(&certificate, "leaf1", 57400, Some("CC")).unwrap();
        assert_eq!(changed.state, TrustState::Mismatch);
        assert_eq!(changed.previous.as_deref(), Some("CC"));
        let verified = PresentedCertificate {
            verification_error: None,
            ..certificate
        };
        assert!(certificate_challenge(&verified, "leaf1", 57400, None).is_none());
    }
}
