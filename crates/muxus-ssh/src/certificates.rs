//! OpenSSH user certificates (CertificateFile): parsing, pairing with the IdentityFile private key
//! and the signature algorithms to offer.

use russh::keys::ssh_key::public::KeyData;
use russh::keys::{Certificate, PrivateKey};

/// A parsed `*-cert-v01@openssh.com` public line.
#[derive(Debug, Clone)]
pub struct OpenSshCertificate {
    pub certificate: Certificate,
    /// The certificate type, e.g. `ssh-ed25519-cert-v01@openssh.com`.
    pub cert_type: String,
    /// The plain key type it certifies, e.g. `ssh-ed25519`.
    pub base_type: String,
    /// The full certificate blob as sent to the server.
    pub public_blob: Vec<u8>,
}

/// Certificate key types a CertificateFile may hold.
fn certificate_base_type(cert_type: &str) -> Option<&str> {
    let base = cert_type
        .strip_suffix("-cert-v01@openssh.com")
        .or_else(|| cert_type.strip_suffix("-cert-v00@openssh.com"))?;
    matches!(
        base,
        "ssh-rsa"
            | "ssh-dss"
            | "ssh-ed25519"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
    )
    .then_some(base)
}

fn is_base64(text: &str) -> bool {
    let body = text.trim_end_matches('=');
    text.len() - body.len() <= 2
        && !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// Parse the public line written by ssh-keygen for an OpenSSH user certificate.
pub fn parse_openssh_certificate(source: &str) -> Result<OpenSshCertificate, String> {
    let line = source.trim();
    let mut fields = line.split_whitespace();
    let (Some(cert_type), Some(blob)) = (fields.next(), fields.next()) else {
        return Err("unsupported certificate format".into());
    };
    if !is_base64(blob) {
        return Err("unsupported certificate format".into());
    }
    let Some(base_type) = certificate_base_type(cert_type) else {
        return Err(format!(
            "unsupported certificate type: {}",
            if cert_type.is_empty() {
                "unknown"
            } else {
                cert_type
            }
        ));
    };
    use base64::Engine;
    let public_blob = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map_err(|_| "malformed OpenSSH certificate".to_string())?;
    let certificate =
        Certificate::from_openssh(line).map_err(|_| "malformed OpenSSH certificate".to_string())?;
    Ok(OpenSshCertificate {
        certificate,
        cert_type: cert_type.to_string(),
        base_type: base_type.to_string(),
        public_blob,
    })
}

/// True when a certificate contains the public half of this private key.
pub fn certificate_matches_key(certificate: &OpenSshCertificate, private_key: &PrivateKey) -> bool {
    certificate_matches_public(certificate, private_key.public_key().key_data())
}

/// True when a certificate certifies this public key.
pub fn certificate_matches_public(certificate: &OpenSshCertificate, key: &KeyData) -> bool {
    certificate.certificate.public_key() == key
}

/// Authentication algorithm variants to try for this certificate.
pub fn certificate_algorithms(certificate: &OpenSshCertificate) -> Vec<String> {
    if certificate.base_type != "ssh-rsa" {
        return vec![certificate.cert_type.clone()];
    }
    let suffix = &certificate.cert_type["ssh-rsa".len()..];
    // Prefer SHA-2, as current OpenSSH disables ssh-rsa/SHA-1 by default, while retaining the
    // SHA-1 variant for older servers.
    vec![
        format!("rsa-sha2-512{suffix}"),
        format!("rsa-sha2-256{suffix}"),
        certificate.cert_type.clone(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Disposable fixture key and user certificate generated only for this test.
    const PRIVATE_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCWM4w+ETsogYZIPXlSJWnKztCQuB+z4IAOiI2ASlUCDQAAAJiYvo8tmL6P
LQAAAAtzc2gtZWQyNTUxOQAAACCWM4w+ETsogYZIPXlSJWnKztCQuB+z4IAOiI2ASlUCDQ
AAAEB7Cv74aU1Jm9orbp5zddvlnDQYRBBOPf5Y2w9d1uvrbZYzjD4ROyiBhkg9eVIlacrO
0JC4H7PggA6IjYBKVQINAAAAE2Zsc2Nod2FyQEwtUEYzUlgyUEsBAg==
-----END OPENSSH PRIVATE KEY-----";

    pub(crate) const CERTIFICATE: &str = "ssh-ed25519-cert-v01@openssh.com AAAAIHNzaC1lZDI1NTE5LWNlcnQtdjAxQG9wZW5zc2guY29tAAAAIMyM6l98PC1DJXz+Dtxy+ONebgpvf/86/nS6ONte+yEXAAAAIJYzjD4ROyiBhkg9eVIlacrO0JC4H7PggA6IjYBKVQINAAAAAAAAAAAAAAABAAAABHRlc3QAAAAIAAAABHRlc3QAAAAAAAAAAP//////////AAAAAAAAAIIAAAAVcGVybWl0LVgxMS1mb3J3YXJkaW5nAAAAAAAAABdwZXJtaXQtYWdlbnQtZm9yd2FyZGluZwAAAAAAAAAWcGVybWl0LXBvcnQtZm9yd2FyZGluZwAAAAAAAAAKcGVybWl0LXB0eQAAAAAAAAAOcGVybWl0LXVzZXItcmMAAAAAAAAAAAAAADMAAAALc3NoLWVkMjU1MTkAAAAgfBX5rZtr934GOp8DZruJhxlEEDuv+RIkW05kWIBoSHcAAABTAAAAC3NzaC1lZDI1NTE5AAAAQO72lXABXCRMOLWJft3FZEg2BzuQ/VF2de09ZPci7i/oE/Z9cOoZj9RjAkfkltBo/aa0S47vjGsU1X+Y9fzEKQc= test";

    #[test]
    fn pairs_a_certificate_file_with_its_identity_file_private_key() {
        let private_key = russh::keys::decode_secret_key(PRIVATE_KEY, None).unwrap();
        let certificate = parse_openssh_certificate(CERTIFICATE).unwrap();
        assert!(certificate_matches_key(&certificate, &private_key));
        assert_eq!(
            certificate_algorithms(&certificate),
            vec!["ssh-ed25519-cert-v01@openssh.com"]
        );
        assert_eq!(certificate.base_type, "ssh-ed25519");
        // The blob is what the server receives as the public key.
        use russh::keys::ssh_encoding::Encode;
        let mut encoded = Vec::new();
        certificate.certificate.encode(&mut encoded).unwrap();
        assert_eq!(encoded, certificate.public_blob);
    }

    #[test]
    fn rejects_a_certificate_for_another_key() {
        let other = russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let certificate = parse_openssh_certificate(CERTIFICATE).unwrap();
        assert!(!certificate_matches_key(&certificate, &other));
    }

    #[test]
    fn rejects_ordinary_public_keys_as_certificate_file_content() {
        assert!(
            parse_openssh_certificate(
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEpS4MTIzNDU2Nzg5MDEyMzQ1Njc4OTAxMjM0NTY="
            )
            .is_err()
        );
    }

    #[test]
    fn offers_sha2_first_for_rsa_certificates() {
        let fake = OpenSshCertificate {
            certificate: parse_openssh_certificate(CERTIFICATE).unwrap().certificate,
            cert_type: "ssh-rsa-cert-v01@openssh.com".into(),
            base_type: "ssh-rsa".into(),
            public_blob: Vec::new(),
        };
        assert_eq!(
            certificate_algorithms(&fake),
            vec![
                "rsa-sha2-512-cert-v01@openssh.com",
                "rsa-sha2-256-cert-v01@openssh.com",
                "ssh-rsa-cert-v01@openssh.com"
            ]
        );
    }
}
