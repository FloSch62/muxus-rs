//! Streaming UTF-8 decoding with the semantics of
//! `new TextDecoder('utf-8', { fatal: false })` used with `{ stream: true }`:
//! a character split across chunks is completed by the next chunk, and
//! invalid bytes become U+FFFD (maximal subparts, as `from_utf8_lossy` does).

#[derive(Debug, Default, Clone)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode a chunk, holding back an unfinished character at its end.
    pub fn decode(&mut self, chunk: &[u8]) -> String {
        if self.pending.is_empty() {
            let keep = incomplete_suffix_len(chunk);
            let text = String::from_utf8_lossy(&chunk[..chunk.len() - keep]).into_owned();
            self.pending.extend_from_slice(&chunk[chunk.len() - keep..]);
            return text;
        }
        self.pending.extend_from_slice(chunk);
        let keep = incomplete_suffix_len(&self.pending);
        let complete = self.pending.len() - keep;
        let text = String::from_utf8_lossy(&self.pending[..complete]).into_owned();
        self.pending.drain(..complete);
        text
    }

    /// End of stream: an unfinished character becomes U+FFFD.
    pub fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

/// Length of a trailing sequence that is a valid but unfinished UTF-8 prefix.
fn incomplete_suffix_len(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for back in 1..=3.min(len) {
        let byte = bytes[len - back];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue;
        }
        let needed = match byte {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        if needed <= back {
            return 0;
        }
        // Only hold back what could still become a valid character.
        let tail = &bytes[len - back..];
        let second_ok = tail.get(1).is_none_or(|&second| match byte {
            0xe0 => (0xa0..=0xbf).contains(&second),
            0xed => (0x80..=0x9f).contains(&second),
            0xf0 => (0x90..=0xbf).contains(&second),
            0xf4 => (0x80..=0x8f).contains(&second),
            _ => (0x80..=0xbf).contains(&second),
        });
        return if second_ok { back } else { 0 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_characters_split_across_chunks() {
        let mut stream = Utf8Stream::new();
        let check = "✓€".as_bytes();
        assert_eq!(stream.decode(&check[..1]), "");
        assert_eq!(stream.decode(&check[1..4]), "✓");
        assert_eq!(stream.decode(&check[4..]), "€");
        assert_eq!(stream.finish(), "");
    }

    #[test]
    fn replaces_invalid_bytes() {
        let mut stream = Utf8Stream::new();
        assert_eq!(stream.decode(b"a\xffb\xe2\x82"), "a\u{fffd}b");
        assert_eq!(stream.finish(), "\u{fffd}");
        assert_eq!(stream.decode(b"\xe0\x80x"), "\u{fffd}\u{fffd}x");
    }
}
