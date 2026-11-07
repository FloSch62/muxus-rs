//! Link detection (Monaco's `links: true`): `http://`, `https://` and `file://` links found by
//! Monaco's `LinkComputer` (`editor/common/languages/linkComputer.ts`), underlined, and opened
//! with Ctrl/Cmd+click.

use std::ops::Range;

/// A detected link: its byte range in the document and the URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedLink {
    pub range: Range<usize>,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Invalid,
    Start,
    H,
    Ht,
    Htt,
    Http,
    F,
    Fi,
    Fil,
    BeforeColon,
    AfterColon,
    AlmostThere,
    End,
    Accept,
}

/// The scheme recognizer: `http:`, `https:` or `file:`, then `//`.
fn next_state(state: State, c: char) -> State {
    use State::*;
    match (state, c.to_ascii_lowercase()) {
        (Start, 'h') => H,
        (Start, 'f') => F,
        (H, 't') => Ht,
        (Ht, 't') => Htt,
        (Htt, 'p') => Http,
        (Http, 's') => BeforeColon,
        (Http, ':') => AfterColon,
        (F, 'i') => Fi,
        (Fi, 'l') => Fil,
        (Fil, 'e') => BeforeColon,
        (BeforeColon, ':') => AfterColon,
        (AfterColon, '/') => AlmostThere,
        (AlmostThere, '/') => End,
        _ => Invalid,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    None,
    ForceTermination,
    CannotEndIn,
}

fn classify(c: char) -> Class {
    const FORCE_TERMINATION: &str = " \t<>'\"、。｡､，．：；‘〈「『〔（［｛｢｣｝］）〕』」〉’｀～…|";
    if FORCE_TERMINATION.contains(c) {
        Class::ForceTermination
    } else if ".,;:".contains(c) {
        Class::CannotEndIn
    } else {
        Class::None
    }
}

/// `_createLink`: drop trailing `.,;:`, and a closing bracket that matches one just before the
/// link.
fn create_link(line: &[(usize, char)], begin: usize, end: usize) -> (usize, usize) {
    let mut last = end - 1;
    loop {
        if classify(line[last].1) != Class::CannotEndIn {
            break;
        }
        last -= 1;
        if last <= begin {
            break;
        }
    }
    if begin > 0 {
        let before = line[begin - 1].1;
        let last_char = line[last].1;
        if matches!((before, last_char), ('(', ')') | ('[', ']') | ('{', '}')) {
            last -= 1;
        }
    }
    (begin, last + 1)
}

/// The links of one line (without its line break), as char index ranges.
fn line_links(line: &[(usize, char)]) -> Vec<(usize, usize)> {
    let mut links = Vec::new();
    let mut begin = 0;
    let mut begin_char = '\0';
    let mut state = State::Start;
    let mut open_parens = false;
    let mut open_square = false;
    let mut in_square = false;
    let mut open_curly = false;
    for (j, &(_, c)) in line.iter().enumerate() {
        let mut reset = false;
        match state {
            State::Accept => {
                let class = match c {
                    '(' => {
                        open_parens = true;
                        Class::None
                    }
                    ')' if open_parens => Class::None,
                    ')' => Class::ForceTermination,
                    '[' => {
                        in_square = true;
                        open_square = true;
                        Class::None
                    }
                    ']' => {
                        in_square = false;
                        if open_square {
                            Class::None
                        } else {
                            Class::ForceTermination
                        }
                    }
                    '{' => {
                        open_curly = true;
                        Class::None
                    }
                    '}' if open_curly => Class::None,
                    '}' => Class::ForceTermination,
                    // Quotes stay inside a link only when another quote opened it.
                    '\'' | '"' | '`' => {
                        if begin_char == c {
                            Class::ForceTermination
                        } else if matches!(begin_char, '\'' | '"' | '`') {
                            Class::None
                        } else {
                            Class::ForceTermination
                        }
                    }
                    '*' if begin_char == '*' => Class::ForceTermination,
                    '*' => Class::None,
                    ' ' if in_square => Class::None,
                    ' ' => Class::ForceTermination,
                    _ => classify(c),
                };
                if class == Class::ForceTermination {
                    links.push(create_link(line, begin, j));
                    reset = true;
                }
            }
            State::End => {
                let class = if c == '[' {
                    // An IPv6 authority has brackets.
                    open_square = true;
                    Class::None
                } else {
                    classify(c)
                };
                if class == Class::ForceTermination {
                    reset = true;
                } else {
                    state = State::Accept;
                }
            }
            _ => {
                state = next_state(state, c);
                if state == State::Invalid {
                    reset = true;
                }
            }
        }
        if reset {
            state = State::Start;
            open_parens = false;
            open_square = false;
            open_curly = false;
            begin = j + 1;
            begin_char = c;
        }
    }
    if state == State::Accept {
        links.push(create_link(line, begin, line.len()));
    }
    links
}

/// Every link in `text`.
pub fn detect_links(text: &str) -> Vec<DetectedLink> {
    let mut links = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let content = line.strip_suffix('\r').unwrap_or(line);
        let chars: Vec<(usize, char)> = content.char_indices().collect();
        for (begin, end) in line_links(&chars) {
            let start = chars[begin].0;
            let stop = chars.get(end).map_or(content.len(), |(offset, _)| *offset);
            links.push(DetectedLink {
                range: line_start + start..line_start + stop,
                url: content[start..stop].to_string(),
            });
        }
        line_start += line.len() + 1;
    }
    links
}

/// The link at `offset`, if any.
pub fn link_at(links: &[DetectedLink], offset: usize) -> Option<&DetectedLink> {
    links
        .iter()
        .find(|link| link.range.start <= offset && offset < link.range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        detect_links(text)
            .into_iter()
            .map(|link| link.url)
            .collect()
    }

    #[test]
    fn finds_http_https_and_file_links() {
        assert_eq!(
            urls(
                "UPSTREAM: http://cache-01:6379\nsee https://example.com/a?b=1. and FILE:///etc/hosts"
            ),
            vec![
                "http://cache-01:6379",
                "https://example.com/a?b=1",
                "FILE:///etc/hosts"
            ]
        );
        // Other schemes are not links; a scheme glued to a word still is.
        assert!(urls("ftp://host mailto:x@y").is_empty());
        assert_eq!(urls("xhttp://a"), vec!["http://a"]);
    }

    #[test]
    fn keeps_balanced_brackets_and_quotes_out() {
        assert_eq!(urls("(see http://a.b/c)"), vec!["http://a.b/c"]);
        assert_eq!(
            urls("http://a.b/wiki/Foo_(bar) end"),
            vec!["http://a.b/wiki/Foo_(bar)"]
        );
        assert_eq!(
            urls(r#"test: ["CMD", "http://localhost:8080/healthz"]"#),
            vec!["http://localhost:8080/healthz"]
        );
        assert_eq!(urls("'http://a/it\"s'"), vec!["http://a/it\"s"]);
        assert_eq!(urls("http://[::1]:80/x"), vec!["http://[::1]:80/x"]);
        assert_eq!(urls("http://a/[b c]/d e"), vec!["http://a/[b c]/d"]);
    }

    #[test]
    fn reports_byte_ranges_across_lines() {
        let text = "é http://a\r\nhttp://b";
        let links = detect_links(text);
        assert_eq!(links[0].range, 3..11);
        assert_eq!(&text[links[1].range.clone()], "http://b");
        assert_eq!(link_at(&links, 5).map(|l| l.url.as_str()), Some("http://a"));
        assert_eq!(link_at(&links, 11), None);
    }
}
