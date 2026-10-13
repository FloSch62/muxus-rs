//! Port of `tests/unit/server/login-sequence-matcher.test.ts`.

use std::time::{Duration, Instant};

use muxus_session::login_sequence::{
    LoginOutputBuffer, MatchError, OutputMatch, TerminalTextStream, WaitPattern,
    compile_wait_pattern,
};

fn literal(text: &str) -> WaitPattern {
    compile_wait_pattern(text, false).unwrap()
}

fn regex(source: &str) -> WaitPattern {
    compile_wait_pattern(source, true).unwrap()
}

#[test]
fn drops_colours_cursor_movement_titles_and_carriage_returns() {
    let mut stream = TerminalTextStream::new();
    assert_eq!(
        stream.write(b"\x1b]0;router: console\x07\x1b[1;33mWelcome\x1b[0m\r\n\x1b[2K\x1b(BPress \x1b[7mRETURN\x1b[m"),
        "Welcome\nPress RETURN"
    );
}

#[test]
fn keeps_escape_sequences_and_characters_that_arrive_split_across_chunks() {
    let mut stream = TerminalTextStream::new();
    let euro = "€".as_bytes();
    let parts: [&[u8]; 7] = [
        b"Pass\x1b[",
        b"1;3",
        b"2mword\x1b]2;ti",
        b"tle\x1b",
        b"\\: ",
        &euro[..1],
        &euro[1..],
    ];
    let text: String = parts.iter().map(|part| stream.write(part)).collect();
    assert_eq!(text, "Password: €");
}

#[test]
fn renders_cursor_forward_as_the_spaces_it_skips() {
    assert_eq!(
        TerminalTextStream::new().write(b"Press\x1b[CRETURN to\x1b[3Cstart"),
        "Press RETURN to   start"
    );
    assert_eq!(TerminalTextStream::new().write(b"a\x1b[500Cb").len(), 82);
}

#[test]
fn finds_literal_text_split_across_chunks_and_through_ansi_codes() {
    let mut buffer = LoginOutputBuffer::default();
    let password = literal("Password:");
    buffer.append(b"banner\r\nPass");
    assert_eq!(buffer.find(&password), Ok(None));
    buffer.append(b"\x1b[1mwo");
    assert_eq!(buffer.find(&password), Ok(None));
    buffer.append(b"rd\x1b[0m: ");
    assert_eq!(
        buffer.find(&password),
        Ok(Some(OutputMatch {
            end: "banner\nPassword:".len()
        }))
    );
}

#[test]
fn matches_regular_expressions_against_the_stripped_text() {
    let mut buffer = LoginOutputBuffer::default();
    buffer.append(b"\x1b[32mcore-sw1\x1b[0m>");
    assert_eq!(
        buffer.find(&regex(r"^\w+-sw\d+[>#]$")),
        Ok(Some(OutputMatch {
            end: "core-sw1>".len()
        }))
    );
    assert_eq!(buffer.find(&regex("#$")), Ok(None));
    // Literal text is matched exactly, never as a pattern.
    assert_eq!(buffer.find(&literal(r"sw\d")), Ok(None));
}

#[test]
fn consumes_a_match_so_the_next_wait_only_sees_later_output() {
    let mut buffer = LoginOutputBuffer::default();
    let login = literal("login:");
    buffer.append(b"login: login: ");
    let first = buffer.find(&login).unwrap().unwrap();
    buffer.consume(first.end);
    assert_eq!(buffer.content(), " login: ");
    let second = buffer.find(&login).unwrap().unwrap();
    buffer.consume(second.end);
    assert_eq!(buffer.find(&login), Ok(None));
}

#[test]
fn keeps_only_a_bounded_window_of_the_newest_output() {
    let mut buffer = LoginOutputBuffer::new(64);
    for index in 0..100 {
        buffer.append(format!("noise line {index}\r\n").as_bytes());
    }
    assert!(buffer.content().len() <= 64);
    assert_eq!(buffer.find(&literal("noise line 0\n")), Ok(None));
    buffer.append(b"Username: ");
    assert!(buffer.find(&literal("Username:")).unwrap().is_some());
    // A match that straddles the trimmed edge is still found after the trim.
    let mut edge = LoginOutputBuffer::new(16);
    edge.append(b"0123456789Pass");
    edge.append(b"word: ");
    assert!(edge.find(&literal("Password:")).unwrap().is_some());
}

#[test]
fn abandons_a_catastrophically_backtracking_pattern_instead_of_stalling() {
    let mut buffer = LoginOutputBuffer::new(8192);
    buffer.append(format!("{}!", "a".repeat(40)).as_bytes());
    let started = Instant::now();
    // Plain patterns run on a linear-time engine and simply do not match.
    assert_eq!(buffer.find(&regex("^(a+)+$")), Ok(None));
    // Lookarounds need the backtracking engine, which gives up instead.
    assert_eq!(
        buffer.find(&regex("^((?=a)a+)+$")),
        Err(MatchError::PatternTimeout)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    // The buffer is still usable afterwards.
    assert_eq!(
        buffer.find(&regex("a!$")),
        Ok(Some(OutputMatch { end: 41 }))
    );
}

#[test]
fn reports_patterns_that_do_not_compile() {
    assert!(compile_wait_pattern("([", true).is_err());
    assert!(compile_wait_pattern("([", false).is_ok());
}
