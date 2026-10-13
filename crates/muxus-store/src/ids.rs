//! Random identifiers in the forms the TypeScript build generates.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// nanoid's URL-safe alphabet (64 symbols, so a byte masked to 6 bits picks
/// one without bias).
const NANOID_ALPHABET: &[u8; 64] =
    b"useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";

/// Fill a buffer from the operating system's random source.
pub fn random_bytes(buffer: &mut [u8]) {
    getrandom::fill(buffer).expect("the operating system random source is unavailable");
}

/// A 21-character nanoid, the row id format of every Muxus table.
pub fn nanoid() -> String {
    let mut bytes = [0u8; 21];
    random_bytes(&mut bytes);
    bytes
        .iter()
        .map(|byte| NANOID_ALPHABET[(byte & 63) as usize] as char)
        .collect()
}

/// `randomBytes(n).toString('base64url')`.
pub fn random_base64url(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    random_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanoids_are_url_safe() {
        let id = nanoid();
        assert_eq!(id.len(), 21);
        assert!(id.bytes().all(|byte| NANOID_ALPHABET.contains(&byte)));
        assert_ne!(nanoid(), id);
        assert_eq!(random_base64url(16).len(), 22);
    }
}
