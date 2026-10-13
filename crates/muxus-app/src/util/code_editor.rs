//! Syntax highlighting for every code editor (the remote editor, the management
//! workbench): gpui-component's tree-sitter grammars with fold ranges. The colors are
//! `muxus_ui::syntax` (Monaco's `vs` and `vs-dark` themes).

use std::ops::Range;
use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::{
    EditorState, FoldIconRenderer, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};
use muxus_ui::gpui_kit::component::highlighter::{LanguageRegistry, SyntaxHighlighter};
use muxus_ui::gpui_kit::{Context, HighlightStyle, SharedString, Window};
use muxus_ui::gpui_kit::{
    ElementId, InteractiveElement, IntoElement, ParentElement, Styled, div, px,
};

/// Files past this size are shown without syntax colors: they are parsed on
/// the UI thread.
pub const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;

/// gpui-component's tree-sitter highlighter, parsed in full on every change.
struct TreeSitterHighlighter {
    inner: SyntaxHighlighter,
    /// The document's brackets with their color, found in the syntax tree after each parse.
    brackets: Vec<(usize, BracketColor)>,
    /// `independentColorPoolPerBracketType`: each kind counts its own depth.
    pools: BracketPools,
}

/// Whether the three bracket kinds share one depth (Monaco's default) or each counts its
/// own (`independentColorPoolPerBracketType`, the remote editor's setting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BracketPools {
    Shared,
    PerKind,
}

/// Monaco's bracket pair colorization: one depth across all three bracket kinds, three
/// colors in turn, and a color of its own for a bracket that closes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BracketColor {
    Depth(usize),
    Unexpected,
}

impl BracketColor {
    fn token(self) -> &'static str {
        match self {
            BracketColor::Depth(depth) => match depth % 3 {
                0 => "bracket.depth.1",
                1 => "bracket.depth.2",
                _ => "bracket.depth.3",
            },
            BracketColor::Unexpected => "bracket.unexpected",
        }
    }
}

/// Colors for `brackets` (byte offset and the bracket) in document order, as Monaco pairs
/// them: an opening bracket gets the depth it opens, a closing one the depth of the bracket it
/// closes, a closing one that matches nothing is unexpected (and an unclosed opening one
/// keeps its depth).
pub fn color_brackets(
    brackets: &[(usize, char)],
    pools: BracketPools,
) -> Vec<(usize, BracketColor)> {
    let mut open: Vec<char> = Vec::new();
    // The depth a bracket of `kind` sits at among the brackets still open.
    let depth_of = |open: &[char], kind: char| match pools {
        BracketPools::Shared => open.len(),
        BracketPools::PerKind => open.iter().filter(|&&c| c == kind).count(),
    };
    brackets
        .iter()
        .map(|&(offset, bracket)| {
            let color = match bracket {
                '(' | '[' | '{' => {
                    let depth = depth_of(&open, bracket);
                    open.push(bracket);
                    BracketColor::Depth(depth)
                }
                _ => {
                    let wanted = match bracket {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if open.last() == Some(&wanted) {
                        open.pop();
                        BracketColor::Depth(depth_of(&open, wanted))
                    } else {
                        BracketColor::Unexpected
                    }
                }
            };
            (offset, color)
        })
        .collect()
}

/// The bracket tokens of a syntax tree (strings and comments hold none: their brackets are
/// not tokens of their own).
fn tree_brackets(node: tree_sitter::Node, out: &mut Vec<(usize, char)>) {
    if node.child_count() == 0 {
        if !node.is_named() || node.kind().len() == 1 {
            let mut chars = node.kind().chars();
            if let (Some(c @ ('(' | ')' | '[' | ']' | '{' | '}')), None) =
                (chars.next(), chars.next())
            {
                out.push((node.start_byte(), c));
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        tree_brackets(child, out);
    }
}

fn to_tree_sitter(edit: InputEdit) -> tree_sitter::InputEdit {
    tree_sitter::InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: tree_sitter::Point::new(
            edit.start_position.row,
            edit.start_position.column,
        ),
        old_end_position: tree_sitter::Point::new(
            edit.old_end_position.row,
            edit.old_end_position.column,
        ),
        new_end_position: tree_sitter::Point::new(
            edit.new_end_position.row,
            edit.new_end_position.column,
        ),
    }
}

impl InputHighlighter for TreeSitterHighlighter {
    fn language(&self) -> SharedString {
        self.inner.language().clone()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.inner.update(edit.map(to_tree_sitter), text, None);
        let mut found = Vec::new();
        if let Some(tree) = self.inner.tree() {
            tree_brackets(tree.root_node(), &mut found);
        }
        self.brackets = color_brackets(&found, self.pools);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut styles = self.inner.styles(range, resolver);
        let first = self
            .brackets
            .partition_point(|(offset, _)| *offset < range.start);
        let colored = self.brackets[first..]
            .iter()
            .take_while(|(offset, _)| *offset < range.end)
            .filter_map(|&(offset, color)| {
                resolver
                    .style(color.token())
                    .map(|style| (offset..offset + 1, style))
            })
            .collect::<Vec<_>>();
        if !colored.is_empty() {
            // The bracket colors take the place of whatever colored the bracket before.
            styles.retain(|(range, _)| {
                !colored
                    .iter()
                    .any(|(bracket, _)| range.start == bracket.start && range.end == bracket.end)
            });
            styles.extend(colored);
            styles.sort_by_key(|(range, _)| (range.start, range.end));
        }
        styles
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        let Some(tree) = self.inner.tree() else {
            return Vec::new();
        };
        let mut ranges = Vec::new();
        collect_folds(tree.root_node(), &mut ranges, true);
        ranges.sort_by_key(|range| range.start_line);
        ranges.dedup_by_key(|range| range.start_line);
        ranges
    }
}

/// Nodes spanning three or more lines fold, as the component's adapter folds.
fn collect_folds(node: tree_sitter::Node, ranges: &mut Vec<FoldRange>, root: bool) {
    let start = node.start_position().row;
    let end = node.end_position().row;
    if !root {
        if end.saturating_sub(start) < 2 {
            return;
        }
        ranges.push(FoldRange::new(start, end));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_folds(child, ranges, false);
    }
}

/// Folding chevrons as Monaco's `showFoldingControls: 'mouseover'` shows them: a folded
/// range always, an open one only while the pointer is over it (the engine would also keep
/// the cursor line's).
pub fn mouseover_fold_icons(dark: bool) -> FoldIconRenderer {
    let ink = muxus_ui::syntax::MonacoColors::new(dark).line_number;
    Rc::new(move |index, folded| {
        let glyph = if folded {
            "ChevronRight"
        } else {
            "KeyboardArrowDown"
        };
        let chevron = div().child(muxus_ui::icon::icon(glyph, 14., ink));
        let chevron = if folded {
            chevron
        } else {
            chevron
                .invisible()
                .group_hover("fold-icon", |s| s.visible())
        };
        div()
            .id(ElementId::NamedInteger("fold-icon".into(), index as u64))
            .group("fold-icon")
            .size(px(14.))
            .child(chevron)
            .into_any_element()
    })
}

/// The highlighter factory the editor states use; `pools` as the editor's
/// `bracketPairColorization` sets them.
pub fn highlighter_factory(pools: BracketPools) -> InputHighlighterFactory {
    Rc::new(move |language: &str| {
        if language == "text" || language.is_empty() {
            return None;
        }
        // Grammars register on first use; a language without one is plain text.
        let registry = LanguageRegistry::singleton();
        let known = registry.language(language).is_some();
        known.then(|| {
            Box::new(TreeSitterHighlighter {
                inner: SyntaxHighlighter::new(language),
                brackets: Vec::new(),
                pools,
            }) as Box<dyn InputHighlighter>
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_brackets_by_depth_across_kinds() {
        let text = r#"{"a": [1, {"b": (2)}], "c": [ ]}"#;
        let brackets: Vec<(usize, char)> = text
            .char_indices()
            .filter(|(_, c)| "()[]{}".contains(*c))
            .collect();
        let colors: Vec<BracketColor> = color_brackets(&brackets, BracketPools::Shared)
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        use BracketColor::Depth;
        assert_eq!(
            colors,
            vec![
                Depth(0), // {
                Depth(1), // [
                Depth(2), // {
                Depth(3), // (
                Depth(3), // )
                Depth(2), // }
                Depth(1), // ]
                Depth(1), // [
                Depth(1), // ]
                Depth(0), // }
            ]
        );
        assert_eq!(BracketColor::Depth(3).token(), "bracket.depth.1");
        // Each kind on its own: `[` inside `{` is the first `[`.
        let per_kind: Vec<BracketColor> = color_brackets(&brackets, BracketPools::PerKind)
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        assert_eq!(
            per_kind,
            vec![
                Depth(0), // {
                Depth(0), // [
                Depth(1), // {
                Depth(0), // (
                Depth(0), // )
                Depth(1), // }
                Depth(0), // ]
                Depth(0), // [
                Depth(0), // ]
                Depth(0), // }
            ]
        );
    }

    #[test]
    fn marks_a_bracket_that_closes_nothing() {
        let colors = color_brackets(
            &[(0, '('), (1, ']'), (2, ')'), (3, '}')],
            BracketPools::Shared,
        );
        assert_eq!(
            colors.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
            vec![
                BracketColor::Depth(0),
                BracketColor::Unexpected,
                BracketColor::Depth(0),
                BracketColor::Unexpected
            ]
        );
    }

    #[test]
    fn finds_brackets_in_the_tree_but_not_in_strings() {
        let mut parser = tree_sitter::Parser::new();
        let registry = LanguageRegistry::singleton();
        let Some(language) = registry
            .language("json")
            .and_then(|config| config.language.clone())
        else {
            return;
        };
        parser.set_language(&language).unwrap();
        let text = r#"{"a": "(not a bracket)", "b": [1]}"#;
        let tree = parser.parse(text, None).unwrap();
        let mut found = Vec::new();
        tree_brackets(tree.root_node(), &mut found);
        let chars: String = found.iter().map(|(_, c)| *c).collect();
        assert_eq!(chars, "{[]}");
    }
}
