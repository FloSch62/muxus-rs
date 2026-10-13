//! Where a search hit sits in a transcript preview (port of
//! `client/src/session-history-matches.ts`).
//!
//! The history store searches with SQLite FTS; the dialog then highlights the
//! token phrases that made a transcript chunk match. Whitespace-separated
//! query parts are independent AND phrases, and the last useful part matches
//! as a prefix unless it is a single character.
//!
//! Offsets count UTF-16 code units, like the TypeScript and like the
//! transcript chunks of [`muxus_core::session_transcript`].
//! [`utf16_to_byte`] turns them into byte offsets for slicing.

use std::sync::LazyLock;

use muxus_core::session_transcript::SessionTranscriptChunk;
use regex::Regex;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// A highlighted range, in UTF-16 code units, end exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptMatch {
    pub start: usize,
    pub end: usize,
}

const fn m(start: usize, end: usize) -> TranscriptMatch {
    TranscriptMatch { start, end }
}

struct TranscriptToken {
    start: usize,
    end: usize,
    normalized: String,
}

struct QueryPhrase {
    tokens: Vec<String>,
    prefix_last_token: bool,
}

static TOKEN_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("a valid pattern"));
static SIGNAL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]").expect("a valid pattern"));

/// Locate the token phrases that make `text` satisfy the FTS query.
pub fn find_transcript_matches(text: &str, query: &str, limit: usize) -> Vec<TranscriptMatch> {
    find_transcript_matches_in_chunks(
        &[SessionTranscriptChunk {
            text: text.to_owned(),
            offset: 0,
            prefixes: Vec::new(),
        }],
        query,
        limit,
    )
}

/// Match each transcript chunk independently, as FTS does. This keeps terms
/// split across adjacent preview events from being presented as one hit.
pub fn find_transcript_matches_in_chunks(
    chunks: &[SessionTranscriptChunk],
    query: &str,
    limit: usize,
) -> Vec<TranscriptMatch> {
    if limit < 1 {
        return Vec::new();
    }
    let phrases = query_phrases(query);
    if phrases.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for chunk in chunks {
        let found = find_chunk_matches(&chunk.text, &phrases, limit - matches.len());
        matches.extend(found.into_iter().map(|found| TranscriptMatch {
            start: display_offset(chunk, found.start, true),
            end: display_offset(chunk, found.end, false),
        }));
        if matches.len() >= limit {
            break;
        }
    }
    matches
}

/// Prefixes belong before a match start, but not after its exclusive end.
fn display_offset(chunk: &SessionTranscriptChunk, offset: usize, inclusive: bool) -> usize {
    let mut result = chunk.offset + offset;
    for prefix in &chunk.prefixes {
        if prefix.offset > offset || (!inclusive && prefix.offset == offset) {
            break;
        }
        result += prefix.length;
    }
    result
}

fn find_chunk_matches(text: &str, phrases: &[QueryPhrase], limit: usize) -> Vec<TranscriptMatch> {
    let tokens = tokenize(text);
    let mut matches = Vec::new();
    for phrase in phrases {
        let mut found = false;
        if tokens.len() >= phrase.tokens.len() {
            for index in 0..=tokens.len() - phrase.tokens.len() {
                if !phrase_matches(&tokens, index, phrase) {
                    continue;
                }
                found = true;
                matches.push(m(
                    tokens[index].start,
                    tokens[index + phrase.tokens.len() - 1].end,
                ));
            }
        }
        if !found {
            return Vec::new();
        }
    }
    matches.sort_by(|left, right| left.start.cmp(&right.start).then(left.end.cmp(&right.end)));
    let mut distinct: Vec<TranscriptMatch> = Vec::new();
    for found in matches {
        if let Some(previous) = distinct.last_mut()
            && found.start < previous.end
        {
            previous.end = previous.end.max(found.end);
            continue;
        }
        if distinct.len() >= limit {
            break;
        }
        distinct.push(found);
    }
    distinct
}

fn query_phrases(query: &str) -> Vec<QueryPhrase> {
    let parts: Vec<&str> = muxus_core::js::js_trim(query)
        .split(muxus_core::js::is_js_whitespace)
        .filter(|part| SIGNAL_PATTERN.is_match(part))
        .collect();
    let last = parts.len().saturating_sub(1);
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| {
            let tokens: Vec<String> = tokenize(part).into_iter().map(|t| t.normalized).collect();
            (!tokens.is_empty()).then(|| QueryPhrase {
                tokens,
                // `part.length > 1` in UTF-16 units.
                prefix_last_token: index == last && part.encode_utf16().count() > 1,
            })
        })
        .collect()
}

fn phrase_matches(tokens: &[TranscriptToken], start: usize, phrase: &QueryPhrase) -> bool {
    phrase.tokens.iter().enumerate().all(|(index, token)| {
        let value = &tokens[start + index].normalized;
        if phrase.prefix_last_token && index == phrase.tokens.len() - 1 {
            value.starts_with(token.as_str())
        } else {
            value == token
        }
    })
}

/// `normalize('NFD')`, marks removed, lower-cased.
fn normalize_token(token: &str) -> String {
    token
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

fn tokenize(value: &str) -> Vec<TranscriptToken> {
    let mut tokens = Vec::new();
    // Walk the matches in order, counting UTF-16 units up to each.
    let mut units = 0usize;
    let mut byte = 0usize;
    for found in TOKEN_PATTERN.find_iter(value) {
        units += value[byte..found.start()].encode_utf16().count();
        let start = units;
        units += found.as_str().encode_utf16().count();
        byte = found.end();
        tokens.push(TranscriptToken {
            start,
            end: units,
            normalized: normalize_token(found.as_str()),
        });
    }
    tokens
}

/// The byte offset of a UTF-16 offset in `text` (clamped to its length; an
/// offset inside a surrogate pair rounds down to the character's start).
pub fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut units = 0usize;
    for (byte, c) in text.char_indices() {
        let next = units + c.len_utf16();
        if next > offset {
            return byte;
        }
        units = next;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use muxus_core::session_transcript::TranscriptPrefix;

    use super::*;

    #[test]
    fn highlights_nonadjacent_query_phrases_independently() {
        assert_eq!(
            find_transcript_matches("BGP neighbor established\n", "BGP established", 100),
            [m(0, 3), m(13, 24)]
        );
    }

    #[test]
    fn uses_exact_tokens_except_for_the_final_prefix_phrase() {
        assert_eq!(
            find_transcript_matches(
                "foo foobar established establishment\n",
                "foo establish",
                100
            ),
            [m(0, 3), m(11, 22), m(23, 36)]
        );
    }

    #[test]
    fn matches_punctuation_separated_token_phrases_and_drops_separators() {
        assert_eq!(
            find_transcript_matches("ssh admin@192.168.72 -> ready\n", "192.168.7 ->", 100),
            [m(10, 20)]
        );
    }

    #[test]
    fn coalesces_overlapping_phrases_and_respects_the_render_limit() {
        assert_eq!(find_transcript_matches("foo foo\n", "foo fo", 1), [m(0, 3)]);
    }

    #[test]
    fn does_not_combine_query_phrases_from_separate_transcript_chunks() {
        let chunks = [
            SessionTranscriptChunk {
                text: "BGP neighbor\n".into(),
                offset: 0,
                prefixes: Vec::new(),
            },
            SessionTranscriptChunk {
                text: "established\n".into(),
                offset: 13,
                prefixes: Vec::new(),
            },
        ];
        assert!(find_transcript_matches_in_chunks(&chunks, "BGP established", 100).is_empty());
    }

    #[test]
    fn folds_accents_and_counts_utf16_units() {
        // "😀" is two UTF-16 units, so "Café" starts at 3.
        assert_eq!(find_transcript_matches("😀 Café\n", "cafe", 10), [m(3, 7)]);
        assert_eq!(utf16_to_byte("😀 Café", 3), 5);
        assert_eq!(utf16_to_byte("😀 Café", 7), "😀 Café".len());
    }

    #[test]
    fn shifts_matches_past_inserted_prefixes() {
        let chunks = [SessionTranscriptChunk {
            text: "status ok\n".into(),
            offset: 10,
            prefixes: vec![TranscriptPrefix {
                offset: 0,
                length: 2,
            }],
        }];
        assert_eq!(
            find_transcript_matches_in_chunks(&chunks, "status", 10),
            [m(12, 18)]
        );
    }
}
