//! The vault's cryptography, byte-compatible with the TypeScript build:
//! scrypt for the master key, AES-256-GCM (12-byte nonce, 16-byte tag) for
//! every sealed value, HMAC-SHA256 for the key check.

use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::VaultError;
use crate::ids::random_bytes;

pub(super) const KEY_BYTES: usize = 32;
pub(super) const SALT_BYTES: usize = 16;
pub(super) const NONCE_BYTES: usize = 12;
pub(super) const TAG_BYTES: usize = 16;

pub(super) const LEGACY_MASTER_KEY_AAD: &[u8] = b"muxus:password-vault:master-key:v2";
pub(super) const MASTER_KEY_AAD: &[u8] = b"muxus:password-vault:master-key:v3";
const KEY_CHECK_CONTEXT: &[u8] = b"muxus:password-vault:key-check:v1";

/// A 32-byte key that is wiped when dropped.
pub(super) type SecretKey = Zeroizing<[u8; KEY_BYTES]>;

/// A sealed value: nonce, ciphertext and authentication tag.
pub(super) struct Sealed {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub auth_tag: Vec<u8>,
}

/// scrypt parameters (`N`, `r`, `p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordVaultKdf {
    pub cost: u32,
    pub block_size: u32,
    pub parallelism: u32,
}

/// The parameters new vaults use: N = 2^17, r = 8, p = 1.
pub const DEFAULT_KDF: PasswordVaultKdf = PasswordVaultKdf {
    cost: 128 * 1024,
    block_size: 8,
    parallelism: 1,
};

/// The associated data binding a credential's ciphertext to its reference id.
pub(super) fn credential_aad(id: &str) -> Vec<u8> {
    format!("muxus:ssh-password:{id}:v1").into_bytes()
}

pub(super) fn random_key() -> SecretKey {
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    random_bytes(key.as_mut());
    key
}

pub(super) fn seal(
    plaintext: &[u8],
    key: &[u8; KEY_BYTES],
    aad: &[u8],
) -> Result<Sealed, VaultError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| VaultError::Crypto("invalid key length".into()))?;
    let mut nonce = vec![0u8; NONCE_BYTES];
    random_bytes(&mut nonce);
    let mut buffer = plaintext.to_vec();
    let tag = cipher
        .encrypt_inout_detached(
            &Nonce::try_from(nonce.as_slice()).expect("12-byte nonce"),
            aad,
            buffer.as_mut_slice().into(),
        )
        .map_err(|_| VaultError::Crypto("encryption failed".into()))?;
    Ok(Sealed {
        nonce,
        ciphertext: buffer,
        auth_tag: tag.to_vec(),
    })
}

/// Authenticate and decrypt; `None` when the key, data or tag do not match.
pub(super) fn open(
    nonce: &[u8],
    ciphertext: &[u8],
    auth_tag: &[u8],
    key: &[u8],
    aad: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    if key.len() != KEY_BYTES || nonce.len() != NONCE_BYTES || auth_tag.len() != TAG_BYTES {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let nonce = Nonce::try_from(nonce).ok()?;
    let tag = Tag::try_from(auth_tag).ok()?;
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    cipher
        .decrypt_inout_detached(&nonce, aad, buffer.as_mut_slice().into(), &tag)
        .ok()?;
    Some(buffer)
}

/// scrypt with Node's parameter rules: `N` must be a power of two above 1.
pub(super) fn derive_key(
    password: &str,
    salt: &[u8],
    kdf: PasswordVaultKdf,
) -> Result<SecretKey, VaultError> {
    if kdf.cost < 2 || !kdf.cost.is_power_of_two() || kdf.block_size == 0 || kdf.parallelism == 0 {
        return Err(VaultError::Kdf("Invalid scrypt params".into()));
    }
    let params = scrypt::Params::new(
        kdf.cost.trailing_zeros() as u8,
        kdf.block_size,
        kdf.parallelism,
    )
    .map_err(|_| VaultError::Kdf("Invalid scrypt params".into()))?;
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    let message = Zeroizing::new(password.as_bytes().to_vec());
    scrypt::scrypt(&message, salt, &params, key.as_mut())
        .map_err(|_| VaultError::Kdf("Invalid scrypt params".into()))?;
    Ok(key)
}

/// HMAC-SHA256 of a fixed context under the vault key: proves a key from the
/// OS credential store is this vault's key without decrypting anything.
pub(super) fn vault_key_check(key: &[u8; KEY_BYTES]) -> Vec<u8> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(KEY_CHECK_CONTEXT);
    mac.finalize().into_bytes().to_vec()
}

/// Constant-time equality.
pub(super) fn same(left: &[u8], right: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens_with_the_same_associated_data() {
        let key = random_key();
        let sealed = seal(b"secret", &key, b"aad").unwrap();
        assert_eq!((sealed.nonce.len(), sealed.auth_tag.len()), (12, 16));
        let opened = open(
            &sealed.nonce,
            &sealed.ciphertext,
            &sealed.auth_tag,
            key.as_ref(),
            b"aad",
        )
        .unwrap();
        assert_eq!(opened.as_slice(), b"secret");
        assert!(
            open(
                &sealed.nonce,
                &sealed.ciphertext,
                &sealed.auth_tag,
                key.as_ref(),
                b"other"
            )
            .is_none()
        );
    }

    #[test]
    fn refuses_scrypt_costs_node_refuses() {
        let kdf = |cost| PasswordVaultKdf {
            cost,
            block_size: 8,
            parallelism: 1,
        };
        assert!(
            derive_key("password", &[0; 16], kdf(1025))
                .unwrap_err()
                .to_string()
                .contains("scrypt")
        );
        assert!(derive_key("password", &[0; 16], kdf(1024)).is_ok());
    }
}
