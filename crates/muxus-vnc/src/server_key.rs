//! RSA-AES servers identify themselves with an RSA key. VNC keys are almost never certified,
//! so first contact and changes are the user's call, as for RDP certificates: the key is shown
//! once and pinned by fingerprint.
//!
//! Port of `vncServerKey` (`client/src/remote-desktop/vnc-client.ts`) and the decision in
//! `DesktopSession.acceptServerKey` (`server/src/remote-desktop/desktop-socket.ts`).

use muxus_core::protocol::{DesktopCertificateChallenge, DesktopIdentity, TrustState};
use sha2::{Digest, Sha256};

/// The key an RSA-AES server presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VncServerKey {
    pub bits: u32,
    /// Colon-separated upper-case SHA-256 of the key as the server sent it; the value pinned.
    pub fingerprint: String,
    /// The first 8 bytes of the SHA-1, the fingerprint TigerVNC's viewer shows.
    pub signature: String,
}

/// Fingerprints of an RSA-AES server key as it travels: the 4-byte key length in bits, then
/// the modulus and exponent, as the server sent them.
pub fn vnc_server_key(public_key: &[u8]) -> VncServerKey {
    let bits = public_key
        .get(..4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .unwrap_or(0);
    let sha256 = Sha256::digest(public_key);
    let sha1 = sha1::Sha1::digest(public_key);
    VncServerKey {
        bits,
        fingerprint: sha256
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
        signature: sha1[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("-"),
    }
}

/// What the user must decide about a server key, or `None` when it is the one pinned before
/// (`pinned` is that fingerprint).
pub fn server_key_challenge(
    key: &VncServerKey,
    host: &str,
    port: u16,
    pinned: Option<&str>,
) -> Option<DesktopCertificateChallenge> {
    if pinned == Some(key.fingerprint.as_str()) {
        return None;
    }
    Some(DesktopCertificateChallenge {
        host: host.to_owned(),
        port,
        fingerprint: key.fingerprint.clone(),
        state: if pinned.is_some() {
            TrustState::Mismatch
        } else {
            TrustState::New
        },
        previous: pinned.map(str::to_owned),
        identity: DesktopIdentity::RsaKey {
            bits: key.bits,
            signature: key.signature.clone(),
        },
    })
}

/// The subject a trusted key is pinned under, for the user's list of trusted identities.
pub fn server_key_subject(key: &VncServerKey) -> String {
    format!("RSA {}-bit key {}", key.bits, key.signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_the_key_the_way_it_was_sent_with_the_signature_vnc_servers_show() {
        // RSA-AES sends the key length in bits, then modulus and exponent padded to the key
        // size.
        let mut sent = 2048u32.to_be_bytes().to_vec();
        sent.extend((0..256).map(|i| (i * 7 + 3) as u8));
        let mut exponent = vec![0u8; 253];
        exponent.extend([1, 0, 1]);
        sent.extend(exponent);

        let key = vnc_server_key(&sent);
        let sha256 = Sha256::digest(&sent);
        let sha1 = sha1::Sha1::digest(&sent);
        assert_eq!(key.bits, 2048);
        assert_eq!(
            key.fingerprint,
            sha256
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(":")
        );
        assert_eq!(
            key.signature,
            sha1[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join("-")
        );
        // The same shape the backend accepts.
        assert_eq!(key.fingerprint.split(':').count(), 32);
        assert_eq!(key.signature.split('-').count(), 8);
    }

    #[test]
    fn reads_the_key_length_from_the_first_four_bytes() {
        let backing = [0xffu8; 16];
        let mut view = backing;
        view[4..8].copy_from_slice(&[0, 0, 4, 0]);
        assert_eq!(vnc_server_key(&view[4..12]).bits, 1024);
    }

    #[test]
    fn pins_a_key_and_asks_again_when_it_changes() {
        let key = VncServerKey {
            bits: 2048,
            fingerprint: "AA".into(),
            signature: "01-23-45-67-89-ab-cd-ef".into(),
        };
        let first = server_key_challenge(&key, "VNC.lab", 5900, None).unwrap();
        assert_eq!(first.state, TrustState::New);
        assert_eq!(
            first.identity,
            DesktopIdentity::RsaKey {
                bits: 2048,
                signature: "01-23-45-67-89-ab-cd-ef".into()
            }
        );
        assert_eq!(
            server_key_challenge(&key, "VNC.lab", 5900, Some("AA")),
            None
        );
        let changed = server_key_challenge(&key, "VNC.lab", 5900, Some("BB")).unwrap();
        assert_eq!(changed.state, TrustState::Mismatch);
        assert_eq!(changed.previous.as_deref(), Some("BB"));
        assert_eq!(
            server_key_subject(&key),
            "RSA 2048-bit key 01-23-45-67-89-ab-cd-ef"
        );
    }
}
