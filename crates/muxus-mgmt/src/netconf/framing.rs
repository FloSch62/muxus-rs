//! NETCONF over SSH message framing (RFC 6242). Hellos, and every message
//! when either side only speaks base:1.0, end with the `]]>]]>` marker;
//! once both sides offer base:1.1 messages travel as chunks,
//! `\n#<size>\n<data>` … `\n##\n`.
//!
//! Replies can run to many megabytes over a stream of small SSH packets, so
//! the decoder keeps one buffer and remembers how far it already searched
//! instead of scanning everything again for every packet.

use bytes::{Buf, BytesMut};

pub const END_OF_MESSAGE: &str = "]]>]]>";
const EOM: &[u8] = END_OF_MESSAGE.as_bytes();
const NEWLINE: u8 = b'\n';
const HASH: u8 = b'#';

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NetconfFramingError(pub String);

fn framing_error(message: impl Into<String>) -> NetconfFramingError {
    NetconfFramingError(message.into())
}

pub fn encode_end_of_message(message: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + EOM.len());
    out.extend_from_slice(message.as_bytes());
    out.extend_from_slice(EOM);
    out
}

pub fn encode_chunked(message: &str) -> Result<Vec<u8>, NetconfFramingError> {
    let body = message.as_bytes();
    if body.is_empty() {
        return Err(framing_error("cannot frame an empty message"));
    }
    let mut out = format!("\n#{}\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\n##\n");
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    EndOfMessage,
    Chunked,
}

/// Incremental decoder for either framing. `push` returns every message
/// completed by the packet; switch to chunked framing once the hellos have
/// been exchanged.
#[derive(Debug)]
pub struct NetconfDecoder {
    mode: Mode,
    queue: BytesMut,
    max_message_bytes: usize,
    /// End-of-message mode: bytes already searched for the marker.
    searched: usize,
    /// Chunked mode: the message being assembled.
    parts: Vec<u8>,
    /// Chunked mode: at least one chunk of the current message arrived.
    started: bool,
    /// Chunked mode: size of the chunk whose header was read, still waiting for its data.
    pending_chunk: Option<usize>,
}

impl Default for NetconfDecoder {
    fn default() -> Self {
        Self::new(1024 * 1024 * 1024)
    }
}

impl NetconfDecoder {
    pub fn new(max_message_bytes: usize) -> Self {
        Self {
            mode: Mode::EndOfMessage,
            queue: BytesMut::new(),
            max_message_bytes,
            searched: 0,
            parts: Vec::new(),
            started: false,
            pending_chunk: None,
        }
    }

    pub fn use_chunked(&mut self) {
        self.mode = Mode::Chunked;
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, NetconfFramingError> {
        self.queue.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            let message = match self.mode {
                Mode::EndOfMessage => self.next_end_of_message()?,
                Mode::Chunked => self.next_chunked()?,
            };
            match message {
                Some(message) => messages.push(message),
                None => return Ok(messages),
            }
        }
    }

    fn next_end_of_message(&mut self) -> Result<Option<String>, NetconfFramingError> {
        let from = self.searched.saturating_sub(EOM.len() - 1);
        let found = self.queue[from..]
            .windows(EOM.len())
            .position(|window| window == EOM)
            .map(|offset| from + offset);
        let Some(end) = found else {
            self.searched = self.queue.len();
            if self.queue.len() > self.max_message_bytes {
                return Err(framing_error("message too large"));
            }
            return Ok(None);
        };
        let message = self.queue.split_to(end);
        self.queue.advance(EOM.len());
        self.searched = 0;
        Ok(Some(String::from_utf8_lossy(&message).into_owned()))
    }

    fn next_chunked(&mut self) -> Result<Option<String>, NetconfFramingError> {
        loop {
            if let Some(size) = self.pending_chunk {
                if self.queue.len() < size {
                    return Ok(None);
                }
                let data = self.queue.split_to(size);
                self.parts.extend_from_slice(&data);
                self.started = true;
                self.pending_chunk = None;
            }
            // Tolerate whitespace some servers leave between messages.
            if !self.started {
                while let Some(&byte) = self.queue.first() {
                    if !matches!(byte, b' ' | b'\t' | b'\r' | NEWLINE) {
                        break;
                    }
                    if byte == NEWLINE {
                        match self.queue.get(1) {
                            None => return Ok(None),
                            Some(&HASH) => break,
                            Some(_) => {}
                        }
                    }
                    self.queue.advance(1);
                }
            }
            if self.queue.len() < 4 {
                return Ok(None);
            }
            if self.queue[0] != NEWLINE || self.queue[1] != HASH {
                return Err(framing_error("expected a chunk header"));
            }
            if self.queue[2] == HASH {
                if self.queue[3] != NEWLINE {
                    return Err(framing_error("malformed end-of-chunks marker"));
                }
                if !self.started {
                    return Err(framing_error("end-of-chunks without a chunk"));
                }
                self.queue.advance(4);
                let message = String::from_utf8_lossy(&self.parts).into_owned();
                self.parts.clear();
                self.started = false;
                return Ok(Some(message));
            }
            // "\n#" then up to ten digits then "\n".
            let limit = self.queue.len().min(14);
            let Some(newline) = (2..limit).find(|index| self.queue[*index] == NEWLINE) else {
                if self.queue.len() >= 14 {
                    return Err(framing_error("chunk size too long"));
                }
                return Ok(None);
            };
            let header = self.queue.split_to(newline + 1);
            let size_text = String::from_utf8_lossy(&header[2..newline]).into_owned();
            let valid = !size_text.is_empty()
                && size_text.len() <= 10
                && !size_text.starts_with('0')
                && size_text.chars().all(|c| c.is_ascii_digit());
            let size = size_text
                .parse::<u64>()
                .ok()
                .filter(|size| valid && *size <= u64::from(u32::MAX))
                .ok_or_else(|| framing_error(format!("invalid chunk size “{size_text}”")))?;
            let size = size as usize;
            if self.parts.len() + size > self.max_message_bytes {
                return Err(framing_error("message too large"));
            }
            self.pending_chunk = Some(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(decoder: &mut NetconfDecoder, data: &[u8], step: usize) -> Vec<String> {
        let mut messages = Vec::new();
        for chunk in data.chunks(step) {
            messages.extend(decoder.push(chunk).unwrap());
        }
        messages
    }

    #[test]
    fn splits_end_of_message_framing_even_when_the_marker_straddles_packets() {
        let mut stream = encode_end_of_message("<hello/>");
        stream.extend(encode_end_of_message("<rpc-reply/>"));
        for step in [1, 2, 3, 5, 7, 100] {
            assert_eq!(
                feed(&mut NetconfDecoder::default(), &stream, step),
                vec!["<hello/>", "<rpc-reply/>"]
            );
        }
    }

    #[test]
    fn reassembles_chunked_messages_made_of_several_chunks() {
        let mut stream = b"\n#5\n<rpc-\n#9\nreply/>xy\n##\n".to_vec();
        stream.extend(encode_chunked("<ok/>").unwrap());
        for step in [1, 4, 64] {
            let mut decoder = NetconfDecoder::default();
            decoder.use_chunked();
            assert_eq!(
                feed(&mut decoder, &stream, step),
                vec!["<rpc-reply/>xy", "<ok/>"]
            );
        }
    }

    #[test]
    fn keeps_bytes_after_the_hello_for_the_negotiated_framing() {
        let mut decoder = NetconfDecoder::default();
        let mut first = encode_end_of_message("<hello/>");
        first.extend_from_slice(b"\n#4\n<a/>");
        assert_eq!(decoder.push(&first).unwrap(), vec!["<hello/>"]);
        decoder.use_chunked();
        assert_eq!(decoder.push(b"\n##\n").unwrap(), vec!["<a/>"]);
    }

    #[test]
    fn tolerates_whitespace_between_chunked_messages_and_rejects_garbage() {
        let mut decoder = NetconfDecoder::default();
        decoder.use_chunked();
        assert_eq!(decoder.push(b" \r\n\n#3\nabc\n##\n").unwrap(), vec!["abc"]);
        let mut broken = NetconfDecoder::default();
        broken.use_chunked();
        assert!(broken.push(b"\n#x\nabc").is_err());
        assert!(broken.push(b"garbage!").is_err());
        let mut zero = NetconfDecoder::default();
        zero.use_chunked();
        assert!(zero.push(b"\n#0\n").is_err());
        assert!(encode_chunked("").is_err());
    }

    #[test]
    fn limits_message_size() {
        let mut decoder = NetconfDecoder::new(16);
        let error = decoder.push(&[b'a'; 32]).unwrap_err();
        assert!(error.0.contains("too large"));
        let mut chunked = NetconfDecoder::new(16);
        chunked.use_chunked();
        assert!(
            chunked
                .push(b"\n#17\n")
                .unwrap_err()
                .0
                .contains("too large")
        );
    }

    #[test]
    fn stays_linear_on_a_large_message_in_tiny_packets() {
        let body = "x".repeat(512 * 1024);
        let stream = encode_end_of_message(&body);
        let mut decoder = NetconfDecoder::default();
        let messages = feed(&mut decoder, &stream, 7);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].len(), body.len());
    }
}
