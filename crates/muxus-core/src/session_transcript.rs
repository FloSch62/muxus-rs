//! Transcript text for a recorded session (port of
//! `shared/src/session-transcript.ts`): timestamps and direction markers are
//! rendered without changing the recorded text or what search matches.
//!
//! Offsets and lengths count UTF-16 code units, like the TypeScript, because
//! the line timestamps stored with each event use them.

use serde::{Deserialize, Serialize};

use crate::api::{SessionLogDirection, SessionLogEvent};

/// Where an inserted prefix (timestamp, marker) sits inside a chunk's own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPrefix {
    /// UTF-16 offset into the chunk's event text the prefix precedes.
    pub offset: usize,
    /// UTF-16 length of the prefix.
    pub length: usize,
}

/// One event's text and where it landed in the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTranscriptChunk {
    pub text: String,
    /// UTF-16 offset of the chunk (its first prefix included) in the transcript.
    pub offset: usize,
    pub prefixes: Vec<TranscriptPrefix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTranscript {
    pub text: String,
    pub chunks: Vec<SessionTranscriptChunk>,
}

/// Render timestamps without changing recorded text or searchable content.
pub fn session_transcript(
    events: &[SessionLogEvent],
    timestamps: bool,
    markers: bool,
) -> SessionTranscript {
    let mut text = String::new();
    let mut text_units = 0usize;
    let mut line_start = true;
    let mut chunks = Vec::with_capacity(events.len());
    for event in events {
        let marker = if markers {
            match event.direction {
                SessionLogDirection::Input => "› ",
                SessionLogDirection::System => "• ",
                SessionLogDirection::Output => "",
            }
        } else {
            ""
        };
        let mut chunk = SessionTranscriptChunk {
            text: event.text.clone(),
            offset: text_units,
            prefixes: Vec::new(),
        };
        let stamps = event.line_timestamps.as_deref().unwrap_or_default();
        let mut stamp_index = 0;
        let mut recorded_at = event.recorded_at.as_str();
        let mut start = 0usize;
        for line in event.text.split_inclusive('\n') {
            while let Some(stamp) = stamps.get(stamp_index)
                && stamp.offset <= start as u64
            {
                recorded_at = &stamp.recorded_at;
                stamp_index += 1;
            }
            let mut prefix = String::new();
            if timestamps && line_start {
                prefix.push_str(&format!("[{recorded_at}] "));
            }
            if start == 0 {
                prefix.push_str(marker);
            }
            if !prefix.is_empty() {
                let length = prefix.encode_utf16().count();
                chunk.prefixes.push(TranscriptPrefix {
                    offset: start,
                    length,
                });
                text.push_str(&prefix);
                text_units += length;
            }
            text.push_str(line);
            let units = line.encode_utf16().count();
            text_units += units;
            line_start = line.ends_with('\n');
            start += units;
        }
        chunks.push(chunk);
    }
    SessionTranscript { text, chunks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SessionLineTimestamp;

    const FIRST: &str = "2026-09-24T10:00:00.000Z";
    const SECOND: &str = "2026-09-24T10:00:05.000Z";

    fn event(text: &str) -> SessionLogEvent {
        SessionLogEvent {
            sequence: 1,
            recorded_at: FIRST.into(),
            elapsed_ms: 0,
            direction: SessionLogDirection::Output,
            text: text.into(),
            line_timestamps: None,
        }
    }

    fn stamp(offset: u64, recorded_at: &str) -> SessionLineTimestamp {
        SessionLineTimestamp {
            offset,
            recorded_at: recorded_at.into(),
        }
    }

    #[test]
    fn uses_per_line_times_preserves_blank_lines_and_does_not_add_a_trailing_prefix() {
        let events = vec![SessionLogEvent {
            line_timestamps: Some(vec![stamp(0, FIRST), stamp(4, FIRST), stamp(5, SECOND)]),
            ..event("one\n\ntwo\n")
        }];
        assert_eq!(
            session_transcript(&events, true, false).text,
            format!("[{FIRST}] one\n[{FIRST}] \n[{SECOND}] two\n")
        );
        assert_eq!(
            session_transcript(&events, false, false).text,
            "one\n\ntwo\n"
        );
    }

    #[test]
    fn uses_legacy_event_times_and_prefixes_a_partial_line_only_once() {
        let events = vec![
            event("par"),
            SessionLogEvent {
                recorded_at: SECOND.into(),
                ..event("tial\nnext")
            },
        ];
        assert_eq!(
            session_transcript(&events, true, false).text,
            format!("[{FIRST}] partial\n[{SECOND}] next")
        );
    }

    #[test]
    fn keeps_search_offsets_correct_and_records_where_prefixes_sit() {
        let events = vec![SessionLogEvent {
            direction: SessionLogDirection::System,
            ..event("deploy\ncomplete\n")
        }];
        let model = session_transcript(&events, true, true);
        let stamp = format!("[{FIRST}] ");
        assert_eq!(model.text, format!("{stamp}• deploy\n{stamp}complete\n"));
        let chunk = &model.chunks[0];
        assert_eq!(chunk.offset, 0);
        assert_eq!(
            chunk.prefixes,
            vec![
                TranscriptPrefix {
                    offset: 0,
                    length: stamp.len() + 2
                },
                TranscriptPrefix {
                    offset: 7,
                    length: stamp.len()
                },
            ]
        );
        // The chunk text maps back into the transcript by skipping its prefixes.
        let units: Vec<u16> = model.text.encode_utf16().collect();
        let complete_at = chunk.offset + chunk.prefixes[0].length + 7 + chunk.prefixes[1].length;
        assert_eq!(
            String::from_utf16(&units[complete_at..complete_at + 8]).unwrap(),
            "complete"
        );
    }

    #[test]
    fn counts_offsets_in_utf16_units() {
        let events = vec![event("😀\n"), event("x")];
        let model = session_transcript(&events, false, true);
        assert_eq!(model.chunks[1].offset, 3);
    }
}
