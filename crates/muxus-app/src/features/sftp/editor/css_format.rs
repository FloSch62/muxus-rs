//! Format Document for CSS, SCSS and Less as Monaco's CSS worker does it:
//! `vscode-css-languageservice`'s `format` runs js-beautify's CSS beautifier
//! (`css/beautifier.js`, MIT) with Monaco's settings (a new line between selectors and
//! between rules, collapsed braces, empty lines kept, no wrapping, no final new line) and the
//! editor's indentation.

use super::beautify::{InputScanner, Output, OutputOptions, is_js_space, literal};

/// At-rules whose block holds rules (`NESTED_AT_RULE`), and those of them that hold only rules
/// (`CONDITIONAL_GROUP_RULE`).
const NESTED_AT_RULES: &[&str] = &[
    "page",
    "font-face",
    "keyframes",
    "media",
    "supports",
    "document",
];
const CONDITIONAL_GROUP_RULES: &[&str] = &["media", "supports", "document"];
/// Properties whose values go on several lines.
const NON_SEMICOLON_NEWLINE_PROPERTIES: &[&str] = &["grid-template-areas", "grid-template"];

/// `max_preserve_newlines`' default.
const MAX_PRESERVE_NEWLINES: usize = 32786;

/// `/[: ,;{}()[\]\/='"]/`: where a variable or at-rule name ends.
fn name_end(chars: &[char]) -> Option<usize> {
    matches!(
        chars.first(),
        Some(':' | ' ' | ',' | ';' | '{' | '}' | '(' | ')' | '[' | ']' | '/' | '=' | '\'' | '"')
    )
    .then_some(1)
}

struct Beautifier {
    input: InputScanner,
    output: Output,
    indent_level: isize,
    nested_level: usize,
}

impl Beautifier {
    /// `eatString`: up to and with the first of `end` (or a new line), escapes kept.
    fn eat_string(&mut self, end: &str) -> String {
        let mut result = String::new();
        while let Some(c) = self.input.next_char() {
            result.push(c);
            if c == '\\' {
                if let Some(escaped) = self.input.next_char() {
                    result.push(escaped);
                }
            } else if end.contains(c) || c == '\n' {
                break;
            }
        }
        result
    }

    /// `eatWhitespace`: skip blanks, keeping the new lines among them when asked.
    fn eat_whitespace(&mut self, keep_newlines: bool) -> bool {
        let found = self.input.peek(0).is_some_and(is_js_space);
        let mut newlines = 0;
        while self.input.peek(0).is_some_and(is_js_space) {
            let c = self.input.next_char();
            if keep_newlines
                && c == Some('\n')
                && (newlines == 0 || newlines < MAX_PRESERVE_NEWLINES)
            {
                newlines += 1;
                self.output.add_new_line(true);
            }
        }
        found
    }

    /// `foundNestedPseudoClass`: a `{` comes before the declaration ends.
    fn found_nested_pseudo_class(&self) -> bool {
        let mut open = 0;
        let mut index = 1;
        while let Some(c) = self.input.peek(index) {
            match c {
                '{' => return true,
                '(' => open += 1,
                ')' => {
                    if open == 0 {
                        return false;
                    }
                    open -= 1;
                }
                ';' | '}' => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    fn print_string(&mut self, text: &str) {
        self.output.set_indent(self.indent_level, 0);
        self.output.non_breaking_space = true;
        self.output.add_token(text);
    }

    fn preserve_single_space(&mut self, after_space: bool) {
        if after_space {
            self.output.space_before_token = true;
        }
    }

    fn indent(&mut self) {
        self.indent_level += 1;
    }

    fn outdent(&mut self) {
        if self.indent_level > 0 {
            self.indent_level -= 1;
        }
    }

    /// `/\/\*(?:[\s\S]*?)((?:\*\/)|$)/`: a block comment, to its end or the input's.
    fn read_block_comment(&mut self) -> String {
        let start = self.input.position();
        self.input.set_position(start + 2);
        let rest = self.input.read_until(literal("*/"), true);
        let mut comment = String::from("/*");
        comment.push_str(&rest);
        comment
    }

    fn beautify(mut self) -> String {
        let mut paren_level = 0usize;
        let mut inside_rule = false;
        let mut inside_property_value = false;
        let mut entering_conditional_group = false;
        let mut inside_non_nested_at_rule = false;
        let mut inside_scss_map = false;
        let mut inside_non_semicolon_values = false;
        let mut top_character: Option<String> = None;
        loop {
            let whitespace = self.input.read_while(is_js_space);
            let after_space = !whitespace.is_empty();
            let previous = top_character.take();
            let previous = previous.as_deref();
            let Some(first) = self.input.next_char() else {
                break;
            };
            let mut ch = first.to_string();
            if first == '\\' && self.input.has_next() {
                ch.push(self.input.next_char().expect("has next"));
            }
            top_character = Some(ch.clone());
            let peek = self.input.peek(0);
            match ch.as_str() {
                "/" if peek == Some('*') => {
                    // Block comments start on a line of their own.
                    self.output.add_new_line(false);
                    self.input.back();
                    let mut comment = self.read_block_comment();
                    if is_ignore_start(&comment) {
                        comment.push_str(&self.input.read_until(ignore_end, true));
                    }
                    self.print_string(&comment);
                    self.eat_whitespace(true);
                    self.output.add_new_line(false);
                }
                "/" if peek == Some('/') => {
                    // A line comment keeps its place after a rule on the same line.
                    self.output.space_before_token = true;
                    self.input.back();
                    let comment = self.input.read_while_line_comment();
                    self.print_string(&comment);
                    self.eat_whitespace(true);
                }
                "$" => {
                    self.preserve_single_space(after_space);
                    self.print_string("$");
                    let mut variable = self.input.peek_until_after(name_end);
                    if variable.ends_with([' ', ':']) {
                        variable = self
                            .eat_string(": ")
                            .trim_end_matches(is_js_space)
                            .to_string();
                        self.print_string(&variable);
                        self.output.space_before_token = true;
                    }
                    // Might be a Sass variable.
                    if paren_level == 0 && variable.contains(':') {
                        inside_property_value = true;
                        self.indent();
                    }
                }
                "@" => {
                    self.preserve_single_space(after_space);
                    if peek == Some('{') {
                        // Less property mixins `@{...}`.
                        let rest = self.eat_string("}");
                        self.print_string(&format!("@{rest}"));
                    } else {
                        self.print_string("@");
                        let mut name = self.input.peek_until_after(name_end);
                        if name.ends_with([' ', ':']) {
                            name = self
                                .eat_string(": ")
                                .trim_end_matches(is_js_space)
                                .to_string();
                            self.print_string(&name);
                            self.output.space_before_token = true;
                        }
                        if paren_level == 0 && name.contains(':') {
                            // Might be a Less variable.
                            inside_property_value = true;
                            self.indent();
                        } else if NESTED_AT_RULES.contains(&name.as_str()) {
                            self.nested_level += 1;
                            if CONDITIONAL_GROUP_RULES.contains(&name.as_str()) {
                                entering_conditional_group = true;
                            }
                        } else if paren_level == 0 && !inside_property_value {
                            inside_non_nested_at_rule = true;
                        }
                    }
                }
                "#" if peek == Some('{') => {
                    self.preserve_single_space(after_space);
                    let rest = self.eat_string("}");
                    self.print_string(&format!("#{rest}"));
                }
                "{" => {
                    if inside_property_value {
                        inside_property_value = false;
                        self.outdent();
                    }
                    // A non-nested at-rule becomes nested.
                    inside_non_nested_at_rule = false;
                    // Entering a conditional group, only rule sets are allowed; otherwise
                    // declarations too.
                    if entering_conditional_group {
                        entering_conditional_group = false;
                        inside_rule = self.indent_level >= self.nested_level as isize;
                    } else {
                        inside_rule = self.indent_level >= self.nested_level as isize - 1;
                    }
                    if inside_rule
                        && self
                            .output
                            .previous_line()
                            .is_some_and(|line| line.item(-1) != Some("{"))
                    {
                        self.output.ensure_empty_line_above("/", ",");
                    }
                    self.output.space_before_token = true;
                    // Collapsed braces; a mixin's first argument being an object keeps none.
                    if previous == Some("(") {
                        self.output.space_before_token = false;
                    } else if previous != Some(",") {
                        self.indent();
                    }
                    self.print_string("{");
                    self.eat_whitespace(true);
                    self.output.add_new_line(false);
                }
                "}" => {
                    self.outdent();
                    self.output.add_new_line(false);
                    if previous == Some("{") {
                        self.output.trim(true);
                    }
                    if inside_property_value {
                        self.outdent();
                        inside_property_value = false;
                    }
                    self.print_string("}");
                    inside_rule = false;
                    if self.nested_level > 0 {
                        self.nested_level -= 1;
                    }
                    self.eat_whitespace(true);
                    self.output.add_new_line(false);
                    if !self.output.just_added_blankline() && self.input.peek(0) != Some('}') {
                        self.output.add_new_line(true);
                    }
                    if self.input.peek(0) == Some(')') {
                        self.output.trim(true);
                    }
                }
                ":" => {
                    if NON_SEMICOLON_NEWLINE_PROPERTIES
                        .iter()
                        .any(|property| self.input.look_back(property))
                    {
                        inside_non_semicolon_values = true;
                    }
                    if (inside_rule || entering_conditional_group)
                        && !(self.input.look_back("&") || self.found_nested_pseudo_class())
                        && !self.input.look_back("(")
                        && !inside_non_nested_at_rule
                        && paren_level == 0
                    {
                        // `property: value`, or a condition in a conditional group.
                        self.print_string(":");
                        if !inside_property_value {
                            inside_property_value = true;
                            self.output.space_before_token = true;
                            self.eat_whitespace(true);
                            self.indent();
                        }
                    } else {
                        // Sass/Less parent references and nested pseudo-classes take no
                        // space; a space before a pseudo-class means "any child" and stays.
                        if self.input.look_back(" ") {
                            self.output.space_before_token = true;
                        }
                        if self.input.peek(0) == Some(':') {
                            self.input.next_char();
                            self.print_string("::");
                        } else {
                            self.print_string(":");
                        }
                    }
                }
                "\"" | "'" => {
                    let after_quote = matches!(previous, Some("\"" | "'"));
                    self.preserve_single_space(after_quote || after_space);
                    let rest = self.eat_string(&ch);
                    self.print_string(&format!("{ch}{rest}"));
                    self.eat_whitespace(true);
                }
                ";" => {
                    inside_non_semicolon_values = false;
                    if paren_level == 0 {
                        if inside_property_value {
                            self.outdent();
                            inside_property_value = false;
                        }
                        inside_non_nested_at_rule = false;
                        self.print_string(";");
                        self.eat_whitespace(true);
                        // A line comment stays on the declaration's line.
                        if self.input.peek(0) != Some('/') {
                            self.output.add_new_line(false);
                        }
                    } else {
                        self.print_string(";");
                        self.eat_whitespace(true);
                        self.output.space_before_token = true;
                    }
                }
                "(" => {
                    if self.input.look_back("url") {
                        self.print_string("(");
                        self.eat_whitespace(false);
                        paren_level += 1;
                        self.indent();
                        match self.input.next_char() {
                            Some(')' | '"' | '\'') => self.input.back(),
                            Some(c) => {
                                let rest = self.eat_string(")");
                                self.print_string(&format!("{c}{rest}"));
                                if paren_level > 0 {
                                    paren_level -= 1;
                                    self.outdent();
                                }
                            }
                            None => {}
                        }
                    } else {
                        // `with(`: only a look back, without tokens.
                        let space_needed = self.input.look_back("with");
                        self.preserve_single_space(after_space || space_needed);
                        self.print_string("(");
                        if inside_property_value && previous == Some("$") {
                            // A Sass map.
                            self.output.add_new_line(false);
                            inside_scss_map = true;
                        } else {
                            self.eat_whitespace(false);
                            paren_level += 1;
                            self.indent();
                        }
                    }
                }
                ")" => {
                    if paren_level > 0 {
                        paren_level -= 1;
                        self.outdent();
                    }
                    if inside_scss_map && self.input.peek(0) == Some(';') {
                        inside_scss_map = false;
                        self.outdent();
                        self.output.add_new_line(false);
                    }
                    self.print_string(")");
                }
                "," => {
                    self.print_string(",");
                    self.eat_whitespace(true);
                    if (!inside_property_value || inside_scss_map)
                        && paren_level == 0
                        && !inside_non_nested_at_rule
                    {
                        self.output.add_new_line(false);
                    } else {
                        self.output.space_before_token = true;
                    }
                }
                ">" | "+" | "~" if !inside_property_value && paren_level == 0 => {
                    // Combinators keep no space around them.
                    self.print_string(&ch);
                    self.eat_whitespace(false);
                }
                "]" => self.print_string("]"),
                "[" => {
                    self.preserve_single_space(after_space);
                    self.print_string("[");
                }
                "=" => {
                    // No whitespace before or after.
                    self.eat_whitespace(false);
                    self.print_string("=");
                }
                "!" if !self.input.look_back("\\") => {
                    // `!important`.
                    self.output.space_before_token = true;
                    self.print_string("!");
                }
                _ => {
                    let after_quote = matches!(previous, Some("\"" | "'"));
                    self.preserve_single_space(after_quote || after_space);
                    self.print_string(&ch);
                    if !self.output.just_added_newline()
                        && self.input.peek(0) == Some('\n')
                        && inside_non_semicolon_values
                    {
                        self.output.add_new_line(false);
                    }
                }
            }
        }
        self.output.get_code("\n")
    }
}

/// `/* beautify ignore:start */` (`Directives.get_directives`).
fn is_ignore_start(comment: &str) -> bool {
    let Some(inner) = comment
        .strip_prefix("/* beautify")
        .and_then(|rest| rest.strip_suffix(" */"))
    else {
        return false;
    };
    let mut ignore_start = false;
    let mut any = false;
    for directive in inner.split(' ') {
        if directive.is_empty() {
            continue;
        }
        let Some((name, value)) = directive.split_once(':') else {
            return false;
        };
        any = true;
        if name == "ignore" && value == "start" {
            ignore_start = true;
        }
    }
    any && inner.starts_with(' ') && ignore_start
}

/// `/\/\*\sbeautify\signore:end\s\*\//`.
fn ignore_end(chars: &[char]) -> Option<usize> {
    let pattern: Vec<char> = "/*_beautify_ignore:end_*/".chars().collect();
    if chars.len() < pattern.len() {
        return None;
    }
    pattern
        .iter()
        .zip(chars)
        .all(|(&want, &got)| {
            if want == '_' {
                is_js_space(got)
            } else {
                want == got
            }
        })
        .then_some(pattern.len())
}

impl InputScanner {
    /// `/\/\/(?:[^\n\r  ]*)/`: a line comment, without its line break.
    fn read_while_line_comment(&mut self) -> String {
        let mut first = true;
        let mut second = false;
        self.read_while(|c| {
            if first || second {
                let ok = c == '/';
                if first {
                    first = false;
                    second = true;
                } else {
                    second = false;
                }
                return ok;
            }
            !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
        })
    }
}

/// The text formatted as Monaco's Format Document leaves a CSS, SCSS or Less file, with
/// `tab_size` and spaces or tabs; the result's line breaks are `\n`.
pub fn format_css(text: &str, tab_size: usize, insert_spaces: bool) -> String {
    // Line breaks become `\n`; the first line's indentation is the base of every line.
    let source = text.replace("\r\n", "\n").replace('\r', "\n");
    let base: String = source
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let options = OutputOptions::new(tab_size, insert_spaces);
    let beautifier = Beautifier {
        input: InputScanner::new(&source),
        output: Output::new(&options, &base),
        indent_level: 0,
        nested_level: 0,
    };
    beautifier.beautify()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases formatted by the js-beautify Monaco 0.57 bundles, with Monaco's options:
    /// `[input, tabSize, insertSpaces, output]`.
    #[test]
    fn formats_like_monacos_css_worker() {
        let cases: Vec<(String, usize, bool, String)> =
            serde_json::from_str(include_str!("testdata/css_format.json")).unwrap();
        for (input, tab_size, insert_spaces, expected) in cases {
            assert_eq!(
                format_css(&input, tab_size, insert_spaces),
                expected,
                "formatting {input:?}"
            );
        }
    }

    #[test]
    fn recognizes_ignore_directives() {
        assert!(is_ignore_start("/* beautify ignore:start */"));
        assert!(is_ignore_start("/* beautify preserve:end ignore:start */"));
        assert!(!is_ignore_start("/* beautify ignore:end */"));
        assert!(!is_ignore_start("/* ignore:start */"));
    }
}
