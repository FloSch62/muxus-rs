//! Full-text query building and pagination cursors.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use muxus_core::js;

/// The FTS5 expression for a user query (`ftsExpression`), or `None` when the
/// query has no searchable token.
///
/// Tokens without letters or digits tokenize to nothing in unicode61 and
/// would silently turn the whole AND-query into "match nothing", so they are
/// dropped. The final token is treated as a prefix so partially typed
/// commands and IPs ("192.168.7") already surface their sessions.
/// Single-character prefixes expand to a large slice of the token dictionary
/// and cost whole-corpus scans on a full database, so they match exactly
/// instead.
pub(crate) fn fts_expression(value: Option<&str>) -> Option<String> {
    let value = js::js_trim(value?);
    let tokens: Vec<&str> = value
        .split(js::is_js_whitespace)
        .filter(|token| token.chars().any(|c| c.is_alphabetic() || c.is_numeric()))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let last = tokens.len() - 1;
    Some(
        tokens
            .iter()
            .enumerate()
            .map(|(index, token)| {
                let phrase = format!("\"{}\"", token.replace('"', "\"\""));
                if index == last && js::utf16_len(token) > 1 {
                    format!("{phrase}*")
                } else {
                    phrase
                }
            })
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

/// Escape `\`, `%` and `_` for a `LIKE … ESCAPE '\'` pattern.
pub(crate) fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The opaque next-page key: base64url of `JSON.stringify([startedAt, id])`.
pub(crate) fn encode_cursor(started_at: &str, id: &str) -> String {
    let json = serde_json::to_string(&[started_at, id]).expect("strings serialize");
    URL_SAFE_NO_PAD.encode(json)
}

/// The `(startedAt, id)` a cursor names, or `None` for anything unreadable.
pub(crate) fn decode_cursor(value: Option<&str>) -> Option<(String, String)> {
    let value = value.filter(|value| !value.is_empty())?;
    // Buffer.from(…, 'base64url') ignores padding and stray characters.
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let bytes = URL_SAFE_NO_PAD.decode(cleaned).ok()?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let array = parsed.as_array()?;
    match (array.first()?, array.get(1)?) {
        (serde_json::Value::String(started_at), serde_json::Value::String(id)) => {
            Some((started_at.clone(), id.clone()))
        }
        _ => None,
    }
}

/// `'…'` with quotes doubled, for `ATTACH DATABASE`.
pub(crate) fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prefix_and_phrase_expressions() {
        assert_eq!(
            fts_expression(Some("BGP established")).unwrap(),
            "\"BGP\" AND \"established\"*"
        );
        assert_eq!(fts_expression(Some("192.168.7")).unwrap(), "\"192.168.7\"*");
        assert_eq!(fts_expression(Some("ping ->")).unwrap(), "\"ping\"*");
        assert_eq!(fts_expression(Some("a")).unwrap(), "\"a\"");
        assert_eq!(
            fts_expression(Some("say \"hi\"")).unwrap(),
            "\"say\" AND \"\"\"hi\"\"\"*"
        );
        assert_eq!(fts_expression(Some("  -> ")), None);
        assert_eq!(fts_expression(None), None);
    }

    #[test]
    fn cursors_round_trip() {
        let cursor = encode_cursor("2020-01-02T10:00:00.000Z", "abc");
        assert_eq!(
            decode_cursor(Some(&cursor)),
            Some(("2020-01-02T10:00:00.000Z".into(), "abc".into()))
        );
        assert_eq!(decode_cursor(Some("!!!")), None);
        assert_eq!(escape_like("50%_a\\"), "50\\%\\_a\\\\");
    }
}
