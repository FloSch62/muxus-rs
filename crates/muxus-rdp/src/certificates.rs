//! RDP servers almost always present self-signed certificates, so Muxus treats them like SSH
//! host keys: a certificate that chains to a trusted authority and names the host is accepted
//! silently; anything else is shown once and pinned by fingerprint (trust on first use), and a
//! later change is flagged.
//!
//! Port of `server/src/remote-desktop/certificates.ts`. The TLS handshake itself accepts any
//! certificate (verification must reach the user, not abort the handshake); this module then
//! checks the chain against the bundled Mozilla roots plus the operating system's store and
//! describes the outcome the way Node's TLS stack does.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use muxus_core::protocol::{DesktopCertificateChallenge, DesktopIdentity, TrustState};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate, X509Name};

/// A server certificate as presented in the TLS handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedCertificate {
    /// Leaf first, as DER.
    pub chain: Vec<Vec<u8>>,
    /// Colon-separated upper-case SHA-256 of the leaf, the fingerprint that is pinned.
    pub fingerprint: String,
    /// `CN=…, O=…`, as Node prints a certificate's subject.
    pub subject: String,
    pub issuer: String,
    /// `Sep 24 15:47:28 2026 GMT`, as OpenSSL prints validity dates.
    pub valid_from: String,
    pub valid_to: String,
    /// Set when the chain or the host name does not verify.
    pub verification_error: Option<String>,
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

/// What the user must decide about a certificate, or `None` when it can be used without
/// asking: it verifies, or it is the one trusted before (`pinned` is that fingerprint).
pub fn certificate_challenge(
    certificate: &PresentedCertificate,
    host: &str,
    port: u16,
    pinned: Option<&str>,
) -> Option<DesktopCertificateChallenge> {
    let verification_error = certificate.verification_error.as_ref()?;
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
            verification_error: Some(verification_error.clone()),
        },
    })
}

/// Colon-separated upper-case hex, Node's `fingerprint256` format.
pub fn colon_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// SHA-256 of a DER certificate in Node's `fingerprint256` format.
pub fn certificate_fingerprint(der: &[u8]) -> String {
    colon_hex(&Sha256::digest(der))
}

pub(crate) fn crypto_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// Bundled Mozilla roots plus the operating system's store (for AD CS-issued RDP certificates).
pub fn trusted_certificate_authorities() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            // No readable system store leaves the bundled roots in charge.
            let native = rustls_native_certs::load_native_certs();
            roots.add_parsable_certificates(native.certs);
            Arc::new(roots)
        })
        .clone()
}

fn attribute_name(oid: &x509_parser::der_parser::oid::Oid) -> String {
    let registry = x509_parser::objects::oid_registry();
    match x509_parser::objects::oid2abbrev(oid, registry) {
        Ok("Email") => "emailAddress".to_owned(),
        Ok(name) => name.to_owned(),
        Err(_) => oid.to_id_string(),
    }
}

/// `key=value, key=value` in certificate order, values of a repeated key joined with `+`
/// (Node's `distinguishedName` over `getPeerCertificate().subject`).
fn distinguished_name(name: &X509Name<'_>) -> String {
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    for rdn in name.iter_rdn() {
        for attribute in rdn.iter() {
            let key = attribute_name(attribute.attr_type());
            let value = attribute.as_str().map(str::to_owned).unwrap_or_else(|_| {
                String::from_utf8_lossy(attribute.attr_value().data).into_owned()
            });
            match entries.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, values)) => values.push(value),
                None => entries.push((key, vec![value])),
            }
        }
    }
    entries
        .into_iter()
        .map(|(key, values)| format!("{key}={}", values.join("+")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `Sep 24 15:47:28 2026 GMT`, OpenSSL's `ASN1_TIME_print`.
fn openssl_time(time: &x509_parser::time::ASN1Time) -> String {
    let datetime = time.to_datetime();
    let month = datetime.month().to_string();
    format!(
        "{} {:>2} {:02}:{:02}:{:02} {} GMT",
        &month[..3.min(month.len())],
        datetime.day(),
        datetime.hour(),
        datetime.minute(),
        datetime.second(),
        datetime.year()
    )
}

fn subject_alt_names(certificate: &X509Certificate<'_>) -> Vec<String> {
    let Ok(Some(extension)) = certificate.subject_alternative_name() else {
        return Vec::new();
    };
    extension
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some(format!("DNS:{dns}")),
            GeneralName::IPAddress(bytes) => {
                let address: Option<IpAddr> = match bytes.len() {
                    4 => <[u8; 4]>::try_from(*bytes).ok().map(IpAddr::from),
                    16 => <[u8; 16]>::try_from(*bytes).ok().map(IpAddr::from),
                    _ => None,
                };
                address.map(|address| format!("IP Address:{address}"))
            }
            _ => None,
        })
        .collect()
}

fn common_name(certificate: &X509Certificate<'_>) -> Option<String> {
    certificate
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_owned)
}

/// Node's `checkServerIdentity` message for a certificate that does not name `host`.
fn identity_error(host: &str, certificate: &X509Certificate<'_>) -> String {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let alt_names = subject_alt_names(certificate);
    let reason = if bare.parse::<IpAddr>().is_ok() {
        let ips: Vec<&str> = alt_names
            .iter()
            .filter_map(|name| name.strip_prefix("IP Address:"))
            .collect();
        format!("IP: {bare} is not in the cert's list: {}", ips.join(", "))
    } else if alt_names.is_empty() {
        format!(
            "Host: {bare}. is not cert's CN: {}",
            common_name(certificate).unwrap_or_default()
        )
    } else {
        format!(
            "Host: {bare}. is not in the cert's altnames: {}",
            alt_names.join(", ")
        )
    };
    format!("Hostname/IP does not match certificate's altnames: {reason}")
}

/// OpenSSL's verification error codes, which Node reports as `authorizationError`.
fn chain_error(
    error: &CertificateError,
    chain: &[CertificateDer<'_>],
    leaf: &X509Certificate<'_>,
) -> String {
    let self_signed = |certificate: &X509Certificate<'_>| {
        certificate.subject().as_raw() == certificate.issuer().as_raw()
    };
    match error {
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
            "CERT_HAS_EXPIRED".to_owned()
        }
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            "CERT_NOT_YET_VALID".to_owned()
        }
        CertificateError::Revoked => "CERT_REVOKED".to_owned(),
        CertificateError::BadSignature => "CERT_SIGNATURE_FAILURE".to_owned(),
        _ if chain.len() == 1 && self_signed(leaf) => "DEPTH_ZERO_SELF_SIGNED_CERT".to_owned(),
        CertificateError::UnknownIssuer => {
            let root_self_signed = chain
                .last()
                .and_then(|der| X509Certificate::from_der(der.as_ref()).ok())
                .is_some_and(|(_, root)| self_signed(&root));
            if chain.len() > 1 && root_self_signed {
                "SELF_SIGNED_CERT_IN_CHAIN".to_owned()
            } else if chain.len() == 1 {
                "UNABLE_TO_VERIFY_LEAF_SIGNATURE".to_owned()
            } else {
                "UNABLE_TO_GET_ISSUER_CERT_LOCALLY".to_owned()
            }
        }
        other => format!("{other:?}"),
    }
}

/// Describe the certificate chain a server presented, verifying it against `roots` for `host`.
pub fn presented_certificate(
    chain: &[CertificateDer<'_>],
    host: &str,
    roots: Arc<RootCertStore>,
) -> Result<PresentedCertificate, String> {
    let leaf_der = chain.first().ok_or("the server presented no certificate")?;
    let (_, leaf) = X509Certificate::from_der(leaf_der.as_ref())
        .map_err(|err| format!("the server certificate could not be read: {err}"))?;

    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let name = match bare.parse::<IpAddr>() {
        Ok(address) => Some(ServerName::IpAddress(address.into())),
        Err(_) => ServerName::try_from(bare.to_owned()).ok(),
    };
    let verification_error = match (name, verifier_for(roots)) {
        (Some(name), Ok(verifier)) => verifier
            .verify_server_cert(leaf_der, &chain[1..], &name, &[], UnixTime::now())
            .err()
            .map(|err| match err {
                rustls::Error::InvalidCertificate(
                    CertificateError::NotValidForName
                    | CertificateError::NotValidForNameContext { .. },
                ) => identity_error(host, &leaf),
                rustls::Error::InvalidCertificate(error) => chain_error(&error, chain, &leaf),
                other => other.to_string(),
            }),
        (None, _) => Some(identity_error(host, &leaf)),
        (_, Err(err)) => Some(err),
    };

    Ok(PresentedCertificate {
        chain: chain.iter().map(|der| der.as_ref().to_vec()).collect(),
        fingerprint: certificate_fingerprint(leaf_der.as_ref()),
        subject: distinguished_name(leaf.subject()),
        issuer: distinguished_name(leaf.issuer()),
        valid_from: openssl_time(&leaf.validity().not_before),
        valid_to: openssl_time(&leaf.validity().not_after),
        verification_error,
    })
}

fn verifier_for(roots: Arc<RootCertStore>) -> Result<Arc<WebPkiServerVerifier>, String> {
    WebPkiServerVerifier::builder_with_provider(roots, crypto_provider())
        .build()
        .map_err(|err| err.to_string())
}

/// The public key CredSSP binds to: the certificate's `subjectPublicKey` bits.
pub(crate) fn subject_public_key(leaf_der: &[u8]) -> Result<Vec<u8>, String> {
    let (_, certificate) = X509Certificate::from_der(leaf_der)
        .map_err(|err| format!("the server certificate could not be read: {err}"))?;
    Ok(certificate
        .public_key()
        .subject_public_key
        .data
        .as_ref()
        .to_vec())
}

/// Verification happens after the handshake: RDP certificates are usually self-signed, and a
/// failed chain must reach the user, not abort the handshake. Handshake signatures are still
/// checked, so the server must hold the key of the certificate the user trusts.
#[derive(Debug)]
pub(crate) struct DeferredVerifier {
    algorithms: WebPkiSupportedAlgorithms,
}

impl DeferredVerifier {
    pub(crate) fn new() -> Self {
        Self {
            algorithms: crypto_provider().signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for DeferredVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-signed P-256 certificate for the fake RDP server (valid until 2126).
    const TEST_CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----
MIIBijCCAS+gAwIBAgIUPTlx/wKVtLsVO0U3wZfD4rXynqgwCgYIKoZIzj0EAwIw
GTEXMBUGA1UEAwwOcmRwLm11eHVzLnRlc3QwIBcNMjYwOTI0MTU0NzI4WhgPMjEy
NjA4MzExNTQ3MjhaMBkxFzAVBgNVBAMMDnJkcC5tdXh1cy50ZXN0MFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEwQ/R8JYkDXHTekz21csVDCuFJWA0KfI4ds0fhKjE
suY+BdGh9nx39N3oN3431swMBfQ15jQN7bvT19dSyHzVUaNTMFEwHQYDVR0OBBYE
FL+fNkTerPkje0NzQDJTSuK1IMZMMB8GA1UdIwQYMBaAFL+fNkTerPkje0NzQDJT
SuK1IMZMMA8GA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDSQAwRgIhAMRoLMzy
oaU+Ln9UhnFQhUTn1a5vff4YA5VSCwDmo2m+AiEAr36LIvt2vzYXvOcLiD5S2IC8
qFqy+UwGqP02HkIH6Ys=
-----END CERTIFICATE-----";

    fn test_certificate_der() -> CertificateDer<'static> {
        use rustls::pki_types::pem::PemObject;
        CertificateDer::from_pem_slice(TEST_CERTIFICATE.as_bytes()).expect("test certificate")
    }

    fn self_signed() -> PresentedCertificate {
        PresentedCertificate {
            chain: vec![b"leaf".to_vec()],
            fingerprint: "AA:BB".into(),
            subject: "CN=win-build".into(),
            issuer: "CN=win-build".into(),
            valid_from: "Jan  1 00:00:00 2026 GMT".into(),
            valid_to: "Jan  1 00:00:00 2027 GMT".into(),
            verification_error: Some("self-signed certificate".into()),
        }
    }

    #[test]
    fn accepts_a_certificate_that_verifies_without_asking() {
        let verified = PresentedCertificate {
            verification_error: None,
            ..self_signed()
        };
        assert_eq!(
            certificate_challenge(&verified, "win-build", 3389, None),
            None
        );
    }

    #[test]
    fn asks_about_an_unknown_self_signed_certificate() {
        let challenge = certificate_challenge(&self_signed(), "win-build", 3389, None).unwrap();
        assert_eq!(challenge.state, TrustState::New);
        assert_eq!(challenge.fingerprint, "AA:BB");
        assert_eq!(challenge.previous, None);
        assert!(matches!(
            challenge.identity,
            DesktopIdentity::Certificate { verification_error: Some(ref error), .. }
                if error == "self-signed certificate"
        ));
    }

    #[test]
    fn accepts_the_pinned_certificate_and_warns_when_it_changed() {
        assert_eq!(
            certificate_challenge(&self_signed(), "win-build", 3389, Some("AA:BB")),
            None
        );
        let challenge =
            certificate_challenge(&self_signed(), "win-build", 3389, Some("CC:DD")).unwrap();
        assert_eq!(challenge.state, TrustState::Mismatch);
        assert_eq!(challenge.previous.as_deref(), Some("CC:DD"));
    }

    #[test]
    fn sends_sni_only_for_host_names() {
        assert_eq!(
            server_name("win-build.example.com").as_deref(),
            Some("win-build.example.com")
        );
        assert_eq!(server_name("192.0.2.10"), None);
        assert_eq!(server_name("[2001:db8::1]"), None);
    }

    #[test]
    fn describes_a_self_signed_certificate_like_node() {
        let der = test_certificate_der();
        let presented = presented_certificate(
            std::slice::from_ref(&der),
            "127.0.0.1",
            trusted_certificate_authorities(),
        )
        .unwrap();
        assert_eq!(presented.subject, "CN=rdp.muxus.test");
        assert_eq!(presented.issuer, "CN=rdp.muxus.test");
        assert_eq!(presented.valid_from, "Sep 24 15:47:28 2026 GMT");
        assert_eq!(presented.valid_to, "Aug 31 15:47:28 2126 GMT");
        assert_eq!(
            presented.fingerprint,
            colon_hex(&Sha256::digest(der.as_ref()))
        );
        assert_eq!(
            presented.verification_error.as_deref(),
            Some("DEPTH_ZERO_SELF_SIGNED_CERT")
        );
        assert_eq!(presented.chain, [der.as_ref().to_vec()]);
    }

    /// A CA and a leaf it issued for `names`.
    fn issued(names: &[&str]) -> (CertificateDer<'static>, CertificateDer<'static>) {
        issued_by("Muxus Test CA", names)
    }

    fn issued_by(
        ca_name: &str,
        names: &[&str],
    ) -> (CertificateDer<'static>, CertificateDer<'static>) {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, ca_name);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        leaf_params
            .distinguished_name
            .push(DnType::CommonName, names[0]);
        leaf_params
            .distinguished_name
            .push(DnType::OrganizationName, "Muxus");
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
        (ca.der().clone(), leaf.der().clone())
    }

    fn roots(ca: &CertificateDer<'static>) -> Arc<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone()).unwrap();
        Arc::new(roots)
    }

    #[test]
    fn trusts_a_certificate_from_a_trusted_authority_that_names_the_host() {
        let (ca, leaf) = issued(&["rdp.example.test", "10.0.0.5"]);
        let presented =
            presented_certificate(std::slice::from_ref(&leaf), "rdp.example.test", roots(&ca))
                .unwrap();
        assert_eq!(presented.verification_error, None);
        assert_eq!(presented.subject, "CN=rdp.example.test, O=Muxus");
        assert_eq!(presented.issuer, "CN=Muxus Test CA");
        assert_eq!(
            certificate_challenge(&presented, "rdp.example.test", 3389, None),
            None
        );
        // An address the certificate lists verifies too.
        let by_address =
            presented_certificate(std::slice::from_ref(&leaf), "10.0.0.5", roots(&ca)).unwrap();
        assert_eq!(by_address.verification_error, None);
    }

    #[test]
    fn reports_a_certificate_for_another_host_the_way_node_does() {
        let (ca, leaf) = issued(&["rdp.example.test"]);
        let presented = presented_certificate(
            std::slice::from_ref(&leaf),
            "other.example.test",
            roots(&ca),
        )
        .unwrap();
        assert_eq!(
            presented.verification_error.as_deref(),
            Some(
                "Hostname/IP does not match certificate's altnames: Host: other.example.test. \
                 is not in the cert's altnames: DNS:rdp.example.test"
            )
        );
        let by_address =
            presented_certificate(std::slice::from_ref(&leaf), "192.0.2.1", roots(&ca)).unwrap();
        assert_eq!(
            by_address.verification_error.as_deref(),
            Some(
                "Hostname/IP does not match certificate's altnames: IP: 192.0.2.1 is not in the cert's list: "
            )
        );
    }

    #[test]
    fn reports_chains_to_an_unknown_authority() {
        let (ca, leaf) = issued(&["rdp.example.test"]);
        // Trust some other authority only.
        let (other_ca, _) = issued_by("Another CA", &["elsewhere.test"]);
        let untrusted = roots(&other_ca);
        let alone = presented_certificate(
            std::slice::from_ref(&leaf),
            "rdp.example.test",
            untrusted.clone(),
        )
        .unwrap();
        assert_eq!(
            alone.verification_error.as_deref(),
            Some("UNABLE_TO_VERIFY_LEAF_SIGNATURE")
        );
        let with_root = presented_certificate(&[leaf, ca], "rdp.example.test", untrusted).unwrap();
        assert_eq!(
            with_root.verification_error.as_deref(),
            Some("SELF_SIGNED_CERT_IN_CHAIN")
        );
        assert_eq!(with_root.chain.len(), 2);
    }

    #[test]
    fn reads_the_public_key_credssp_binds_to() {
        let key = subject_public_key(test_certificate_der().as_ref()).unwrap();
        // An uncompressed P-256 point.
        assert_eq!(key.len(), 65);
        assert_eq!(key[0], 0x04);
    }
}
