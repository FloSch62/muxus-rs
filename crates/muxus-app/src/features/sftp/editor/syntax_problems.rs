//! Syntax errors from the editor's tree-sitter parse, as problem markers. Monaco reports syntax
//! errors through its language workers (TypeScript for JavaScript and TypeScript, the CSS
//! service for CSS); here the grammar the editor already parses with finds them: a token the
//! parser had to insert (a `MISSING` node) is "expected", tokens it had to skip (an `ERROR`
//! node) are unexpected. The messages follow the worker of the language (TypeScript's, the CSS
//! service's), other languages get the same wording as TypeScript's.
//!
//! Not every grammar is asked: JSON has its own checks, browsers accept HTML that does not
//! parse, SCSS and Less are shown with the CSS grammar, the SQL grammar knows one dialect,
//! C and C++ hide syntax behind macros the grammar cannot expand, and Markdown has no syntax
//! errors.

use std::ops::Range;

use super::markers::{Marker, Severity};

/// No more markers than this per file: a file in the wrong language is all errors.
const MAX_MARKERS: usize = 500;

/// Whose messages a language's syntax errors take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    /// TypeScript's (JavaScript, TypeScript).
    Script,
    /// The CSS language service's.
    Css,
    /// TypeScript's wording for any other language.
    Other,
}

/// Which messages the syntax errors of `grammar` take, for the Monaco language `language`;
/// `None` for languages whose parse is not checked.
fn family(grammar: &str, language: &str) -> Option<Family> {
    match grammar {
        "javascript" | "typescript" | "tsx" => Some(Family::Script),
        "css" if language == "css" => Some(Family::Css),
        "bash" | "python" | "rust" | "go" | "java" | "kotlin" | "csharp" | "lua" | "php"
        | "ruby" | "scala" | "swift" | "yaml" | "graphql" | "proto" | "elixir" => {
            Some(Family::Other)
        }
        _ => None,
    }
}

/// The syntax errors in `tree`, the parse of `text` with `grammar`, for a file in the Monaco
/// language `language`.
pub fn syntax_markers(
    tree: &tree_sitter::Tree,
    text: &str,
    grammar: &str,
    language: &str,
) -> Vec<Marker> {
    let Some(family) = family(grammar, language) else {
        return Vec::new();
    };
    let root = tree.root_node();
    if !root.has_error() {
        return Vec::new();
    }
    let mut markers = Vec::new();
    collect(root, text, family, &mut markers);
    markers
}

fn collect(node: tree_sitter::Node, text: &str, family: Family, out: &mut Vec<Marker>) {
    if out.len() >= MAX_MARKERS {
        return;
    }
    if node.is_missing() {
        out.push(missing(node, text, family));
        return;
    }
    let before = out.len();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() {
            collect(child, text, family, out);
        }
    }
    if out.len() > before {
        return;
    }
    if node.is_error() {
        // Skipped tokens with nothing more precise inside: an error the parser wrapped
        // around much of the file has the real ones nested in it.
        out.push(unexpected(node, text, family));
    } else if node.byte_range().is_empty() && node.parent().is_some() {
        // The error is in a token the grammar keeps hidden (a value's digits): the node
        // itself is what is missing.
        out.push(missing(node, text, family));
    }
}

/// Where an empty range is marked: the next character on its line that is not blank (the
/// token that came instead), else the last one before it.
fn widen(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    for (index, c) in text[offset..].char_indices() {
        match c {
            ' ' | '\t' => continue,
            '\n' | '\r' => break,
            _ => return offset + index..offset + index + c.len_utf8(),
        }
    }
    match text[..offset]
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_whitespace())
    {
        Some((index, c)) => index..index + c.len_utf8(),
        None => offset..offset,
    }
}

/// A quote on `line` that nothing closes, as a byte offset into it.
fn unterminated_quote(line: &str) -> Option<usize> {
    let mut open: Option<(usize, char)> = None;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (open, c) {
            (Some(_), '\\') => escaped = true,
            (Some((_, quote)), c) if c == quote => open = None,
            (None, '\'' | '"' | '`') => open = Some((index, c)),
            _ => {}
        }
    }
    open.map(|(index, _)| index)
}

/// A token the parser inserted: it was expected here.
fn missing(node: tree_sitter::Node, text: &str, family: Family) -> Marker {
    let kind = node.kind();
    let named = node.is_named();
    let (message, code): (String, Option<&str>) = match family {
        Family::Css => {
            let (message, code) = match kind {
                ";" => ("semi-colon expected", "css-semicolonexpected"),
                ":" => ("colon expected", "css-colonexpected"),
                "{" => ("{ expected", "css-lcurlyexpected"),
                "}" => ("} expected", "css-rcurlyexpected"),
                "(" => ("( expected", "css-lparentexpected"),
                ")" => (") expected", "css-rparentexpected"),
                "[" => ("[ expected", "css-rbracketexpected"),
                "]" => ("] expected", "css-lbracketexpected"),
                "," => ("comma expected", "css-commaexpected"),
                _ if kind.ends_with("name") || kind.contains("identifier") => {
                    ("identifier expected", "css-identifierexpected")
                }
                _ if kind.contains("value") => {
                    ("property value expected", "css-propertyvalueexpected")
                }
                _ => ("term expected", "css-termexpected"),
            };
            (message.into(), Some(code))
        }
        Family::Script | Family::Other => {
            if !named {
                (format!("'{kind}' expected."), Some("1005"))
            } else if kind.contains("identifier") || kind.ends_with("name") {
                ("Identifier expected.".into(), Some("1003"))
            } else {
                ("Expression expected.".into(), Some("1109"))
            }
        }
    };
    marker(
        widen(text, node.start_byte()),
        message,
        family,
        code.filter(|_| family != Family::Other),
    )
}

/// Tokens the parser skipped: the first line of them is marked.
fn unexpected(node: tree_sitter::Node, text: &str, family: Family) -> Marker {
    let start = node.start_byte().min(text.len());
    let end = node.end_byte().min(text.len());
    let first_line_end = text[start..end]
        .find('\n')
        .map_or(end, |newline| start + newline);
    let trimmed = start + text[start..first_line_end].trim_end().len();
    let range = if trimmed > start {
        start..trimmed
    } else {
        widen(text, start)
    };
    let parent = node.parent().map(|parent| parent.kind()).unwrap_or("");
    // A string that runs to the end of its line.
    if family == Family::Script
        && let Some(quote) = unterminated_quote(&text[start..first_line_end])
    {
        let line_end = text[start + quote..]
            .find('\n')
            .map_or(text.len(), |newline| start + quote + newline);
        return marker(
            start + quote..line_end,
            "Unterminated string literal.".into(),
            family,
            Some("1002"),
        );
    }
    let (message, code) = match family {
        Family::Css => {
            if matches!(parent, "stylesheet" | "") {
                ("at-rule or selector expected", "css-ruleorselectorexpected")
            } else if text[start..end].trim_start().starts_with(':') {
                ("property value expected", "css-propertyvalueexpected")
            } else {
                ("colon expected", "css-colonexpected")
            }
        }
        Family::Script | Family::Other => {
            let statement_level = matches!(
                parent,
                "" | "program"
                    | "source_file"
                    | "module"
                    | "statement_block"
                    | "block"
                    | "class_body"
                    | "declaration_list"
                    | "compilation_unit"
                    | "chunk"
                    | "stream"
                    | "document"
            );
            if statement_level {
                ("Declaration or statement expected.", "1128")
            } else {
                ("Unexpected token.", "1012")
            }
        }
    };
    marker(
        range,
        message.into(),
        family,
        (family != Family::Other).then_some(code),
    )
}

fn marker(range: Range<usize>, message: String, family: Family, code: Option<&str>) -> Marker {
    Marker {
        range,
        message: message.into(),
        severity: Severity::Error,
        source: (family == Family::Css).then_some("css"),
        code: code.map(|code| code.to_string().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_ui::gpui_kit::component::highlighter::LanguageRegistry;

    fn parse(grammar: &str, text: &str) -> Option<tree_sitter::Tree> {
        let language = LanguageRegistry::singleton()
            .language(grammar)
            .and_then(|config| config.language.clone())?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).ok()?;
        parser.parse(text, None)
    }

    fn markers(grammar: &str, language: &str, text: &str) -> Vec<(String, String)> {
        let Some(tree) = parse(grammar, text) else {
            return Vec::new();
        };
        syntax_markers(&tree, text, grammar, language)
            .into_iter()
            .map(|marker| {
                (
                    text[marker.range.clone()].to_string(),
                    format!(
                        "{}{}",
                        marker.message,
                        marker
                            .details()
                            .map(|d| format!(" {d}"))
                            .unwrap_or_default()
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn valid_files_have_no_markers() {
        assert!(markers("javascript", "javascript", "const a = [1, 2];\nfoo(a);\n").is_empty());
        assert!(markers("css", "css", "a { color: red; }\n").is_empty());
        assert!(markers("python", "python", "def f(x):\n    return x\n").is_empty());
        assert!(markers("yaml", "yaml", "a: 1\nb:\n  - c\n").is_empty());
    }

    #[test]
    fn languages_monaco_does_not_check_this_way_have_none() {
        assert!(markers("html", "html", "<div><p>text</span>").is_empty());
        assert!(markers("css", "scss", "$x: 1;\na { b { color: $x } }").is_empty());
        assert!(markers("json", "json", "{,}").is_empty());
    }

    #[test]
    fn a_missing_token_is_expected() {
        let found = markers("javascript", "javascript", "foo(1, 2;\n");
        assert!(
            found
                .iter()
                .any(|(_, message)| message == "')' expected. (1005)"),
            "{found:?}"
        );
        let found = markers("css", "css", "a b { color: red;\n");
        assert_eq!(
            found,
            vec![(";".into(), "} expected css(css-rcurlyexpected)".into())]
        );
        let found = markers("css", "css", "a { color: ; }\n");
        assert_eq!(
            found,
            vec![(
                ";".into(),
                "property value expected css(css-propertyvalueexpected)".into()
            )]
        );
        // The token that came instead is marked, else the last one before.
        let found = markers("javascript", "javascript", "if (x {\n  y();\n}\n");
        assert_eq!(found, vec![("{".into(), "')' expected. (1005)".into())]);
        let found = markers("bash", "shell", "if true; then\n  echo hi\n");
        assert_eq!(found, vec![("i".into(), "'fi' expected.".into())]);
    }

    #[test]
    fn skipped_tokens_are_unexpected() {
        let found = markers("javascript", "javascript", "let a = 1;\n)))\nlet b = 2;\n");
        assert!(
            found
                .iter()
                .any(|(text, message)| text.contains(')') && message.contains("expected")),
            "{found:?}"
        );
        let found = markers("python", "python", "def f(:\n    pass\n");
        assert_eq!(found, vec![(":".into(), "')' expected.".into())]);
        // An error wrapped around the whole file is reported where it really is.
        let found = markers(
            "tsx",
            "typescript",
            "import { a } from 'a';\nconst o = {\n  b: 'x',\n  c: y\",\n  d: 'z',\n};\n",
        );
        assert!(
            found
                .first()
                .is_some_and(|(text, message)| text.starts_with('"')
                    && message == "Unterminated string literal. (1002)"),
            "{found:?}"
        );
        let found = markers("typescript", "typescript", "const s: string = 'abc;\n");
        assert_eq!(
            found,
            vec![("'abc;".into(), "Unterminated string literal. (1002)".into())]
        );
    }

    #[test]
    fn marks_the_character_where_a_token_is_missing() {
        assert_eq!(widen("ab\ncd", 1), 1..2);
        assert_eq!(widen("a  b", 1), 3..4);
        // At the end of a line, the last character before.
        assert_eq!(widen("ab\ncd", 2), 1..2);
        assert_eq!(widen("ab  \n", 3), 1..2);
        assert_eq!(widen("ab", 2), 1..2);
        assert_eq!(widen("", 0), 0..0);
    }

    #[test]
    fn finds_a_quote_nothing_closes() {
        assert_eq!(unterminated_quote("a = 'b' + \"c"), Some(10));
        assert_eq!(unterminated_quote("a = 'b\\'c'"), None);
        assert_eq!(unterminated_quote("`x`"), None);
    }

    /// Prints what the grammars make of common mistakes (`--ignored --nocapture`).
    #[test]
    #[ignore]
    fn show_trees() {
        if let Ok(file) = std::env::var("MUXUS_TREE_FILE") {
            let text = std::fs::read_to_string(&file).unwrap();
            let grammar = std::env::var("MUXUS_TREE_GRAMMAR").unwrap_or("tsx".into());
            let tree = parse(&grammar, &text).unwrap();
            fn show(node: tree_sitter::Node, depth: usize) {
                if !node.has_error() && depth > 0 {
                    return;
                }
                println!(
                    "{}{} {:?}-{:?} err={} missing={}",
                    "  ".repeat(depth),
                    node.kind(),
                    node.start_position(),
                    node.end_position(),
                    node.is_error(),
                    node.is_missing()
                );
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if depth < 3 || child.has_error() {
                        show(child, depth + 1);
                    }
                }
            }
            show(tree.root_node(), 0);
            for marker in syntax_markers(&tree, &text, &grammar, &grammar) {
                println!(
                    "  {:?} {}",
                    text[marker.range.clone()].to_string(),
                    marker.message
                );
            }
            return;
        }
        for (grammar, text) in [
            ("javascript", "let x = ;\n"),
            ("javascript", "foo(a b);\n"),
            ("javascript", "if (x {\n  y();\n}\n"),
            ("javascript", "function f() {\n  return 1;\n"),
            ("typescript", "const s: string = 'abc;\n"),
            ("css", "a { color red; }\n"),
            ("css", "a { color: ; }\n"),
            ("css", "a b { color: red;\n"),
            ("css", "}\na { }\n"),
            ("python", "def f(:\n    pass\n"),
            ("python", "x = (1,\ny = 2\n"),
            ("bash", "if [ -f x ]; then\n  echo hi\n"),
            ("yaml", "a: [1, 2\nb: 3\n"),
            ("rust", "fn main() {\n    let x = 1\n}\n"),
            ("go", "func main() {\n\tx := \n}\n"),
        ] {
            let tree = parse(grammar, text).unwrap();
            println!("--- {grammar}: {text:?}\n{}", tree.root_node().to_sexp());
            for marker in syntax_markers(&tree, text, grammar, grammar) {
                println!(
                    "  {:?} {:?} {}",
                    marker.range.clone(),
                    &text[marker.range.clone()],
                    marker.message
                );
            }
        }
    }
}
