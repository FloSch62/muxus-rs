//! Shared text matching for host lists. Every query is split into whitespace
//! separated tokens that each have to appear somewhere in the host's searchable
//! text, so "af tail" finds `MyAirframe1Tail` in the `AF-Tails` folder: the
//! same rule the quick launcher already uses for its catalog.

/// Split an already normalized (trimmed, lower-cased) query into tokens.
pub fn search_tokens(normalized: &str) -> Vec<&str> {
    normalized.split_whitespace().collect()
}

pub fn matches_tokens(searchable: &str, tokens: &[&str]) -> bool {
    tokens.iter().all(|token| searchable.contains(token))
}

/// How well a host answers the query, or `None` when it does not. Exact and
/// prefix hits on the name beat hits that only landed in the address, the
/// description or the folder, so Enter picks the host you were spelling out.
pub fn match_score(label: &str, searchable: &str, query: &str, tokens: &[&str]) -> Option<u32> {
    if !matches_tokens(searchable, tokens) {
        return None;
    }
    if label == query {
        return Some(1_000);
    }
    if label.starts_with(query) {
        return Some(800);
    }
    if label.contains(query) {
        return Some(500);
    }
    if matches_tokens(label, tokens) {
        return Some(350);
    }
    Some(100)
}

/// The lower-cased search text of a host: every present field, space-joined.
///
/// The TypeScript caches this per entry object; the Rust callers build it on
/// demand, which costs microseconds for a sidebar's worth of hosts.
pub fn searchable_text<'a>(fields: impl IntoIterator<Item = Option<&'a str>>) -> String {
    fields
        .into_iter()
        .flatten()
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_any_whitespace() {
        assert_eq!(search_tokens("af  tail\tx"), ["af", "tail", "x"]);
        assert!(search_tokens("").is_empty());
    }

    #[test]
    fn scores_name_hits_above_metadata_hits() {
        let tokens = search_tokens("gateway");
        assert_eq!(
            match_score("gateway", "gateway", "gateway", &tokens),
            Some(1_000)
        );
        assert_eq!(
            match_score("gateway-2", "gateway-2", "gateway", &tokens),
            Some(800)
        );
        assert_eq!(
            match_score("lab-gateway", "lab-gateway", "gateway", &tokens),
            Some(500)
        );
        assert_eq!(
            match_score("router", "router gateway", "gateway", &tokens),
            Some(100)
        );
        assert_eq!(match_score("router", "router", "gateway", &tokens), None);
    }

    #[test]
    fn joins_present_fields_lower_cased() {
        assert_eq!(
            searchable_text([Some("Web"), None, Some(""), Some("PROD")]),
            "web prod"
        );
    }
}
