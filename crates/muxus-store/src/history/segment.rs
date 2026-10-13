//! The `.muxlog` raw segment format shared with the TypeScript build.
//!
//! A segment starts with an 8-byte header and holds one frame per recorded
//! event, all big-endian:
//!
//! ```text
//! u32 payload length (25 + raw length)
//! u64 sequence
//! u64 recorded-at, Unix epoch milliseconds
//! u64 elapsed milliseconds
//! u8  direction (1 input, 2 output, 3 system)
//! ... raw bytes
//! ```
//!
//! An active part is appended to as `NNNNNN.muxlog.partial`; a finished part
//! is zstd-compressed to `NNNNNN.muxlog.zst`. A torn final frame (a crash in
//! the middle of a write) ends the decoded events and is truncated away.

use muxus_core::api::SessionLogDirection;
use muxus_core::js;

use super::HistoryEvent;

/// `MUXLOG`, format version 1, newline.
pub(crate) const SEGMENT_HEADER: [u8; 8] = [0x4d, 0x55, 0x58, 0x4c, 0x4f, 0x47, 0x01, 0x0a];

/// Bytes of a frame before its raw payload, length prefix excluded.
const FRAME_FIXED: usize = 25;

fn direction_to_byte(direction: SessionLogDirection) -> u8 {
    match direction {
        SessionLogDirection::Input => 1,
        SessionLogDirection::Output => 2,
        SessionLogDirection::System => 3,
    }
}

fn byte_to_direction(byte: u8) -> Option<SessionLogDirection> {
    match byte {
        1 => Some(SessionLogDirection::Input),
        2 => Some(SessionLogDirection::Output),
        3 => Some(SessionLogDirection::System),
        _ => None,
    }
}

/// One encoded frame (`encodeFrame`).
pub(crate) fn encode_frame(
    sequence: u64,
    recorded_at: &str,
    elapsed_ms: u64,
    direction: SessionLogDirection,
    raw: &[u8],
) -> Vec<u8> {
    let payload_length = FRAME_FIXED + raw.len();
    let mut frame = Vec::with_capacity(4 + payload_length);
    frame.extend_from_slice(&(payload_length as u32).to_be_bytes());
    frame.extend_from_slice(&sequence.to_be_bytes());
    // Date.parse, falling back to now for an unreadable timestamp.
    let recorded = js::parse_timestamp(recorded_at)
        .map(|at| at.timestamp_millis())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    frame.extend_from_slice(&(recorded.max(0) as u64).to_be_bytes());
    frame.extend_from_slice(&elapsed_ms.to_be_bytes());
    frame.push(direction_to_byte(direction));
    frame.extend_from_slice(raw);
    frame
}

/// The events of a segment and where its last complete frame ends.
pub(crate) struct DecodedSegment {
    pub events: Vec<HistoryEvent>,
    pub valid_end: usize,
}

/// Decode a segment (`decodeSegment`). Decoding stops at the first torn or
/// unreadable frame; `valid_end` is the offset just past the last good one.
pub(crate) fn decode_segment(data: &[u8]) -> Result<DecodedSegment, String> {
    if data.len() < SEGMENT_HEADER.len() || data[..SEGMENT_HEADER.len()] != SEGMENT_HEADER {
        return Err("invalid muxlog segment header".into());
    }
    let mut events = Vec::new();
    let mut offset = SEGMENT_HEADER.len();
    while offset + 4 <= data.len() {
        let length =
            u32::from_be_bytes(data[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        if length < FRAME_FIXED || offset + 4 + length > data.len() {
            break;
        }
        let Some(direction) = byte_to_direction(data[offset + 28]) else {
            break;
        };
        let read_u64 =
            |at: usize| u64::from_be_bytes(data[at..at + 8].try_into().expect("8 bytes"));
        let sequence = read_u64(offset + 4);
        let recorded_ms = read_u64(offset + 12);
        let elapsed_ms = read_u64(offset + 20);
        let recorded_at = chrono::DateTime::from_timestamp_millis(recorded_ms as i64)
            .map(js::iso_timestamp)
            .unwrap_or_default();
        events.push(HistoryEvent {
            sequence,
            recorded_at,
            elapsed_ms,
            direction,
            raw: data[offset + 29..offset + 4 + length].to_vec(),
            text: String::new(),
            line_timestamps: None,
        });
        offset += 4 + length;
    }
    Ok(DecodedSegment {
        events,
        valid_end: offset,
    })
}

/// A session id as a directory name (`safeSessionName`): ids made of
/// `[A-Za-z0-9_-]` are used as they are, anything else as base64url.
pub(crate) fn safe_session_name(id: &str) -> String {
    use base64::Engine;
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        id.to_owned()
    } else {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id.as_bytes())
    }
}

/// A part number as its six-digit file name stem (`partName`).
pub(crate) fn part_name(part: u32) -> String {
    format!("{part:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_torn_tails_stop_decoding() {
        let mut segment = SEGMENT_HEADER.to_vec();
        segment.extend(encode_frame(
            1,
            "2026-01-01T10:00:01.000Z",
            1000,
            SessionLogDirection::Output,
            b"hello",
        ));
        let good = segment.len();
        segment.extend_from_slice(&[0, 0, 0, 100, 1, 2, 3]);
        let decoded = decode_segment(&segment).unwrap();
        assert_eq!(decoded.valid_end, good);
        assert_eq!(decoded.events.len(), 1);
        let event = &decoded.events[0];
        assert_eq!(event.raw, b"hello");
        assert_eq!(event.elapsed_ms, 1000);
        assert_eq!(event.recorded_at, "2026-01-01T10:00:01.000Z");
        assert!(decode_segment(b"nope").is_err());
    }

    #[test]
    fn session_names_are_path_safe() {
        assert_eq!(safe_session_name("abc_-9"), "abc_-9");
        assert_eq!(safe_session_name("../x"), "Li4veA");
        assert_eq!(part_name(3), "000003");
    }
}
