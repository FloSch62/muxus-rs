//! Release version precedence (port of `shared/src/update-version.ts`).

use std::cmp::Ordering;
use std::sync::LazyLock;

use regex::Regex;

struct ParsedVersion<'a> {
    core: [&'a str; 3],
    prerelease: Vec<&'a str>,
}

// `[0-9]` rather than `\d`: JavaScript's `\d` is ASCII only.
static SEMVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[vV]?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$",
    )
    .expect("valid regex")
});

fn is_numeric(identifier: &str) -> bool {
    !identifier.is_empty() && identifier.bytes().all(|b| b.is_ascii_digit())
}

fn parse_version(version: &str) -> Option<ParsedVersion<'_>> {
    let captures = SEMVER.captures(crate::js::js_trim(version))?;
    let prerelease: Vec<&str> = captures
        .get(4)
        .map(|m| m.as_str().split('.').collect())
        .unwrap_or_default();
    if prerelease.iter().any(|identifier| {
        is_numeric(identifier) && identifier.len() > 1 && identifier.starts_with('0')
    }) {
        return None;
    }
    let part = |index: usize| captures.get(index).map_or("", |m| m.as_str());
    Some(ParsedVersion {
        core: [part(1), part(2), part(3)],
        prerelease,
    })
}

/// Digit strings without leading zeros compare by length, then lexically.
fn compare_numeric(candidate: &str, current: &str) -> Ordering {
    candidate
        .len()
        .cmp(&current.len())
        .then_with(|| candidate.cmp(current))
}

fn compare_prerelease(candidate: &[&str], current: &[&str]) -> Ordering {
    if candidate.is_empty() || current.is_empty() {
        // A release outranks any of its prereleases.
        return current.len().cmp(&candidate.len());
    }
    for index in 0..candidate.len().max(current.len()) {
        let (Some(next), Some(installed)) = (candidate.get(index), current.get(index)) else {
            return if candidate.len() < current.len() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        };
        if next == installed {
            continue;
        }
        return match (is_numeric(next), is_numeric(installed)) {
            (true, true) => compare_numeric(next, installed),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => next.cmp(installed),
        };
    }
    Ordering::Equal
}

/// True when `candidate` has higher SemVer precedence than `current`.
pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    let (Some(next), Some(installed)) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    for (next_part, installed_part) in next.core.iter().zip(installed.core.iter()) {
        let precedence = compare_numeric(next_part, installed_part);
        if precedence != Ordering::Equal {
            return precedence == Ordering::Greater;
        }
    }
    compare_prerelease(&next.prerelease, &installed.prerelease) == Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_stable_core_versions() {
        assert!(is_newer_version("0.2.0", "0.1.1"));
        assert!(is_newer_version("V10.0.0", "v9.999.999"));
        assert!(!is_newer_version("0.1.1", "0.1.1"));
        assert!(!is_newer_version("0.1.0", "0.1.1"));
    }

    #[test]
    fn offers_the_final_release_to_prerelease_users() {
        assert!(is_newer_version("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer_version("0.2.0-beta.1", "0.2.0"));
    }

    #[test]
    fn uses_semver_prerelease_precedence_and_ignores_build_metadata() {
        assert!(is_newer_version("1.0.0-beta.11", "1.0.0-beta.2"));
        assert!(is_newer_version("1.0.0-rc.1", "1.0.0-beta.11"));
        assert!(!is_newer_version("1.0.0+build.2", "1.0.0+build.1"));
        assert!(is_newer_version("1.0.0-beta.1.1", "1.0.0-beta.1"));
        assert!(is_newer_version("1.0.0-beta", "1.0.0-1"));
    }

    #[test]
    fn rejects_malformed_versions() {
        assert!(!is_newer_version("1.0", "0.9.0"));
        assert!(!is_newer_version("1.0.0-beta.01", "1.0.0-beta.1"));
    }
}
