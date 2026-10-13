// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a
// copy of the MPL was not distributed with this file, You can obtain one at
// https://mozilla.org/MPL/2.0/. Adapted from noVNC (Copyright The noVNC authors).

//! The cryptography of the VNC sign-in methods noVNC supports: VNC authentication (DES),
//! UltraVNC MSLogonII (Diffie-Hellman + DES-CBC), Apple Remote Desktop (Diffie-Hellman + MD5 +
//! AES-128-ECB) and RSA-AES (RSA key exchange, AES-EAX messages).
//!
//! Port of noVNC's `core/crypto/*` and `core/ra2.js` (MPL-2.0) on RustCrypto primitives.

use aes::Aes128;
use des::Des;
use des::cipher::{Block, BlockCipherEncrypt, KeyInit};
use eax::Eax;
use eax::aead::{Aead, Payload};
use md5::{Digest, Md5};
use num_bigint::BigUint;
use rand::Rng;
use rsa::traits::PublicKeyParts;
use rsa::{BoxedUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// VNC's DES reads key bits least significant first (d3des `bytebit`), so a standard DES needs
/// each key byte mirrored.
fn vnc_des(key: [u8; 8]) -> Des {
    let mirrored = key.map(u8::reverse_bits);
    Des::new_from_slice(&mirrored).expect("an 8-byte DES key")
}

fn des_block(cipher: &Des, data: &mut [u8]) {
    let mut block = Block::<Des>::default();
    block.copy_from_slice(data);
    cipher.encrypt_block(&mut block);
    data.copy_from_slice(&block);
}

/// The VNC authentication response: the 16-byte challenge encrypted with the password (its
/// first eight characters, as Latin-1 bytes) as the DES key.
pub fn vnc_auth_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    // `charCodeAt`, low byte: what noVNC's DES key schedule reads.
    for (slot, unit) in key.iter_mut().zip(password.encode_utf16()) {
        *slot = unit as u8;
    }
    let cipher = vnc_des(key);
    let mut response = *challenge;
    for chunk in response.chunks_mut(8) {
        des_block(&cipher, chunk);
    }
    response
}

/// `g^private mod p`, both sides of a Diffie-Hellman exchange, as big-endian bytes padded to
/// `len`.
fn pad_be(value: &BigUint, len: usize) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    if bytes.len() >= len {
        return bytes[bytes.len() - len..].to_vec();
    }
    let mut padded = vec![0u8; len - bytes.len()];
    padded.extend_from_slice(&bytes);
    padded
}

struct DiffieHellman {
    private: BigUint,
    prime: BigUint,
    len: usize,
}

impl DiffieHellman {
    /// A private key as long as the prime, like noVNC's `DHCipher`.
    fn new(prime: &[u8]) -> Self {
        Self {
            private: BigUint::from_bytes_be(&random_bytes(prime.len())),
            prime: BigUint::from_bytes_be(prime),
            len: prime.len(),
        }
    }

    fn public_key(&self, generator: &[u8]) -> Vec<u8> {
        let generator = BigUint::from_bytes_be(generator);
        pad_be(&generator.modpow(&self.private, &self.prime), self.len)
    }

    fn shared_secret(&self, peer: &[u8]) -> Vec<u8> {
        let peer = BigUint::from_bytes_be(peer);
        pad_be(&peer.modpow(&self.private, &self.prime), self.len)
    }
}

/// Bytes of `value`, at most `max`, followed by a NUL, laid over random padding of `len`.
fn padded_field(value: &str, max: usize, len: usize) -> Vec<u8> {
    let mut field = random_bytes(len);
    let bytes = &value.as_bytes()[..value.len().min(max)];
    field[..bytes.len()].copy_from_slice(bytes);
    field[bytes.len()] = 0;
    field
}

/// What an Apple Remote Desktop sign-in sends: the encrypted credentials, then our public key.
pub struct ArdResponse {
    pub credentials: [u8; 128],
    pub public_key: Vec<u8>,
}

/// Apple Remote Desktop (security type 30): Diffie-Hellman with the server's parameters, the
/// MD5 of the shared secret as an AES-128 key, user name and password (63 bytes each at most)
/// in a 128-byte block encrypted in ECB mode.
pub fn ard_response(
    generator: &[u8],
    prime: &[u8],
    server_public_key: &[u8],
    username: &str,
    password: &str,
) -> ArdResponse {
    let dh = DiffieHellman::new(prime);
    let public_key = dh.public_key(generator);
    let key = Md5::digest(dh.shared_secret(server_public_key));
    let mut plain = padded_field(username, 63, 64);
    plain.extend(padded_field(password, 63, 64));
    let cipher = Aes128::new_from_slice(&key).expect("a 16-byte AES key");
    let mut credentials = [0u8; 128];
    for (out, chunk) in credentials.chunks_mut(16).zip(plain.chunks(16)) {
        let mut block = Block::<Aes128>::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        out.copy_from_slice(&block);
    }
    ArdResponse {
        credentials,
        public_key,
    }
}

/// What an UltraVNC MSLogonII sign-in sends: our public key and the encrypted user name and
/// password.
pub struct MsLogonResponse {
    pub public_key: [u8; 8],
    pub username: [u8; 256],
    pub password: [u8; 64],
}

fn des_cbc(key: [u8; 8], data: &mut [u8]) {
    let cipher = vnc_des(key);
    let mut chain = key;
    for chunk in data.chunks_mut(8) {
        for (byte, previous) in chunk.iter_mut().zip(chain) {
            *byte ^= previous;
        }
        des_block(&cipher, chunk);
        chain.copy_from_slice(chunk);
    }
}

/// UltraVNC MSLogonII (security type 113): 64-bit Diffie-Hellman; the shared secret is both
/// the DES-CBC key and its IV.
pub fn ms_logon_response(
    generator: &[u8; 8],
    prime: &[u8; 8],
    server_public_key: &[u8; 8],
    username: &str,
    password: &str,
) -> MsLogonResponse {
    let dh = DiffieHellman::new(prime);
    let mut public_key = [0u8; 8];
    public_key.copy_from_slice(&dh.public_key(generator));
    let mut secret = [0u8; 8];
    secret.copy_from_slice(&dh.shared_secret(server_public_key));
    let mut user = [0u8; 256];
    user.copy_from_slice(&padded_field(username, 255, 256));
    let mut pass = [0u8; 64];
    pass.copy_from_slice(&padded_field(password, 63, 64));
    des_cbc(secret, &mut user);
    des_cbc(secret, &mut pass);
    MsLogonResponse {
        public_key,
        username: user,
        password: pass,
    }
}

/// One direction of an RSA-AES session: AES-128-EAX with a little-endian message counter as
/// the nonce and the message length as associated data.
pub struct Ra2Cipher {
    cipher: Eax<Aes128>,
    counter: [u8; 16],
}

impl Ra2Cipher {
    pub fn new(key: &[u8]) -> Self {
        Self {
            cipher: Eax::<Aes128>::new_from_slice(key).expect("a 16-byte AES key"),
            counter: [0; 16],
        }
    }

    fn advance(&mut self) {
        for byte in &mut self.counter {
            let (next, overflow) = byte.overflowing_add(1);
            *byte = next;
            if !overflow {
                break;
            }
        }
    }

    /// Length, ciphertext and tag for one message.
    pub fn make_message(&mut self, message: &[u8]) -> Vec<u8> {
        let ad = (message.len() as u16).to_be_bytes();
        let nonce = self.counter.into();
        let encrypted = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: message,
                    aad: &ad,
                },
            )
            .expect("EAX encryption cannot fail");
        self.advance();
        let mut out = ad.to_vec();
        out.extend(encrypted);
        out
    }

    /// Decrypt one `length`-byte message (ciphertext and tag); `None` when it does not
    /// authenticate.
    pub fn receive_message(&mut self, length: u16, encrypted: &[u8]) -> Option<Vec<u8>> {
        let ad = length.to_be_bytes();
        let nonce = self.counter.into();
        let plain = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: encrypted,
                    aad: &ad,
                },
            )
            .ok();
        self.advance();
        plain
    }
}

/// RSA public key from big-endian modulus and exponent.
pub fn rsa_public_key(modulus: &[u8], exponent: &[u8]) -> Result<RsaPublicKey, String> {
    RsaPublicKey::new(
        BoxedUint::from_be_slice_vartime(modulus),
        BoxedUint::from_be_slice_vartime(exponent),
    )
    .map_err(|err| format!("RA2: invalid server public key: {err}"))
}

pub fn rsa_encrypt(key: &RsaPublicKey, data: &[u8]) -> Result<Vec<u8>, String> {
    key.encrypt(&mut rand::rng(), Pkcs1v15Encrypt, data)
        .map_err(|err| format!("RA2: {err}"))
}

/// A fresh 2048-bit client key, as RSA-AES clients generate per connection (slow: call off
/// the async runtime).
pub fn rsa_client_key() -> Result<RsaPrivateKey, String> {
    RsaPrivateKey::new(&mut rand::rng(), 2048).map_err(|err| format!("RA2: {err}"))
}

/// The client key as RSA-AES sends it: bit length, then modulus and exponent padded to the
/// key size.
pub fn rsa_public_blob(key: &RsaPrivateKey) -> Vec<u8> {
    let bits = 2048u32;
    let bytes = (bits / 8) as usize;
    let pad = |value: Box<[u8]>| {
        let mut padded = vec![0u8; bytes.saturating_sub(value.len())];
        padded.extend_from_slice(&value);
        padded
    };
    let mut blob = bits.to_be_bytes().to_vec();
    blob.extend(pad(key.n_bytes()));
    blob.extend(pad(key.e_bytes()));
    blob
}

pub fn rsa_decrypt(key: &RsaPrivateKey, data: &[u8]) -> Option<Vec<u8>> {
    key.decrypt(Pkcs1v15Encrypt, data).ok()
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let digest = sha1::Sha1::digest(data);
    let mut out = [0u8; 20];
    out.copy_from_slice(&digest);
    out
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_the_vnc_challenge_like_d3des() {
        // DES-ECB with the bit-mirrored key 0e86ceceeef64e26 (computed with openssl).
        let challenge = *b"0123456789abcdef";
        let response = vnc_auth_response("password", &challenge);
        assert_eq!(
            response,
            [
                0x56, 0x45, 0xab, 0xeb, 0x5f, 0x1e, 0x64, 0x75, 0xe8, 0xfe, 0xb1, 0x1b, 0xeb, 0x66,
                0xea, 0x19
            ]
        );
        assert_ne!(response, vnc_auth_response("passwore", &challenge));
        // Only the first eight characters count.
        assert_eq!(response, vnc_auth_response("password-and-more", &challenge));
    }

    #[test]
    fn mirrors_key_bits_for_vnc_des() {
        // The key schedule reads bit 0 first: a key byte 0x01 is a standard DES 0x80.
        let mut ours = [0u8; 8];
        des_block(&vnc_des([1, 0, 0, 0, 0, 0, 0, 0]), &mut ours);
        let mut standard = [0u8; 8];
        des_block(
            &Des::new_from_slice(&[0x80, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
            &mut standard,
        );
        assert_eq!(ours, standard);
    }

    #[test]
    fn agrees_on_a_diffie_hellman_secret() {
        let prime = 0xffff_ffff_ffff_ffc5u64.to_be_bytes();
        let generator = [2u8];
        let ours = DiffieHellman::new(&prime);
        let theirs = DiffieHellman::new(&prime);
        assert_eq!(
            ours.shared_secret(&theirs.public_key(&generator)),
            theirs.shared_secret(&ours.public_key(&generator))
        );
        assert_eq!(ours.public_key(&generator).len(), 8);
    }

    #[test]
    fn round_trips_ra2_messages() {
        let mut client = Ra2Cipher::new(&[7; 16]);
        let mut server = Ra2Cipher::new(&[7; 16]);
        for message in [&b"hello"[..], &[1u8; 20][..]] {
            let wire = client.make_message(message);
            assert_eq!(&wire[..2], &(message.len() as u16).to_be_bytes());
            assert_eq!(wire.len(), 2 + message.len() + 16);
            assert_eq!(
                server
                    .receive_message(message.len() as u16, &wire[2..])
                    .as_deref(),
                Some(message)
            );
        }
        // A tampered message does not authenticate.
        let mut wire = client.make_message(b"x");
        wire[2] ^= 1;
        assert_eq!(server.receive_message(1, &wire[2..]), None);
    }

    #[test]
    fn pads_credentials_with_a_terminator() {
        let field = padded_field("admin", 63, 64);
        assert_eq!(&field[..6], b"admin\0");
        assert_eq!(field.len(), 64);
        let long = "x".repeat(100);
        assert_eq!(&padded_field(&long, 63, 64)[..64][63], &0);
    }
}
