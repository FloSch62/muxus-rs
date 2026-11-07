//! The language features Monaco gives the remote editor, kept per open file: detected links
//! (underlined; Ctrl/Cmd+hover and click open them), occurrence and selection highlights, and
//! JSON problem markers. They feed the overview ruler and the status bar's problem count.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use muxus_ui::gpui_kit::base::input::{
    DefinitionProvider, Diagnostic, DiagnosticSeverity, EditorState, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, Rope, RopeExt, TextDecoration,
    TextDecorationCollection,
};
use muxus_ui::gpui_kit::{App, Entity, HighlightStyle, Task, UnderlineStyle, Window, px};
use muxus_ui::syntax::MonacoColors;

use super::json::{Problem, Severity, json_problems};
use super::links::{DetectedLink, detect_links, link_at};
use super::occurrences::{MAX_SEARCHED_BYTES, Occurrences, occurrences};
use super::overlays::{Lane, Mark, OverlayData, lines_of};

/// Ctrl/Cmd+hover over a link underlines it, a click opens it (Monaco's link opener, through
/// the editor's go-to-definition gesture).
struct LinkDefinitions(Rc<RefCell<Vec<DetectedLink>>>);

impl DefinitionProvider for LinkDefinitions {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<Vec<lsp_types::LocationLink>>> {
        let links = self.0.borrow();
        let found = link_at(&links, offset).and_then(|link| {
            let target_uri: lsp_types::Uri = link.url.parse().ok()?;
            let range = lsp_types::Range {
                start: text.offset_to_position(link.range.start),
                end: text.offset_to_position(link.range.end),
            };
            Some(lsp_types::LocationLink {
                origin_selection_range: Some(range),
                target_uri,
                target_range: range,
                target_selection_range: range,
            })
        });
        Task::ready(Ok(found.into_iter().collect()))
    }
}

/// One file's links, highlights and problems.
pub struct DocumentFeatures {
    links: Rc<RefCell<Vec<DetectedLink>>>,
    link_decorations: Option<TextDecorationCollection>,
    occurrence_decorations: Option<RangeDecorationCollection>,
    occurrences: Occurrences,
    /// The selection the highlights were found for.
    selection: Option<Range<usize>>,
    problems: Vec<Problem>,
    line_starts: Vec<usize>,
    dark: bool,
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

impl DocumentFeatures {
    /// Hook the link opener into `editor`.
    pub fn install(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let links = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(LinkDefinitions(links.clone()));
        editor.update(cx, |state, _| {
            let lsp = state.lsp_mut();
            lsp.definition_provider = Some(provider);
            // Every link leaves the editor: http(s) and file URLs open with the desktop.
            lsp.show_document = Some(Rc::new(|params, _, cx| {
                cx.open_url(params.uri.as_str());
                true
            }));
        });
        Self {
            links,
            link_decorations: None,
            occurrence_decorations: None,
            occurrences: Occurrences::default(),
            selection: None,
            problems: Vec::new(),
            line_starts: vec![0],
            dark: false,
        }
    }

    /// The text changed (or was loaded): find links and problems again, and the highlights.
    pub fn text_changed(
        &mut self,
        editor: &Entity<EditorState>,
        language: &str,
        dark: bool,
        cx: &mut App,
    ) {
        let rope = editor.read(cx).text().clone();
        let text = rope.to_string();
        self.line_starts = line_starts(&text);
        let links = if text.len() <= MAX_SEARCHED_BYTES {
            detect_links(&text)
        } else {
            Vec::new()
        };
        let underline = HighlightStyle {
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            }),
            ..HighlightStyle::default()
        };
        let decorations: Vec<TextDecoration> = links
            .iter()
            .map(|link| TextDecoration::new(link.range.clone(), underline))
            .collect();
        *self.links.borrow_mut() = links;
        match &self.link_decorations {
            Some(collection) => collection.set(decorations, cx),
            None => {
                self.link_decorations = Some(editor.update(cx, |state, cx| {
                    state.create_decorations_collection(decorations, cx)
                }));
            }
        }
        self.problems = if language == "json" && text.len() <= MAX_SEARCHED_BYTES {
            json_problems(&text)
        } else {
            Vec::new()
        };
        let diagnostics: Vec<Diagnostic> = self
            .problems
            .iter()
            .map(|problem| {
                Diagnostic::new(
                    rope.offset_to_position(problem.range.start)
                        ..rope.offset_to_position(problem.range.end),
                    problem.message,
                )
                .with_severity(match problem.severity {
                    Severity::Error => DiagnosticSeverity::Error,
                    Severity::Warning => DiagnosticSeverity::Warning,
                })
                .with_source("json")
            })
            .collect();
        editor.update(cx, |state, cx| {
            let text = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&text);
                set.extend(diagnostics);
            }
            cx.notify();
        });
        self.selection = None;
        self.cursor_moved(editor, dark, cx);
    }

    /// The cursor or selection moved: highlight what it is on. Whether anything changed.
    pub fn cursor_moved(&mut self, editor: &Entity<EditorState>, dark: bool, cx: &mut App) -> bool {
        let state = editor.read(cx);
        let selection = state.selected_range();
        if self.selection.as_ref() == Some(&selection) && self.dark == dark {
            return false;
        }
        self.selection = Some(selection.clone());
        self.dark = dark;
        let found = if state.text().len() <= MAX_SEARCHED_BYTES {
            occurrences(&state.text().to_string(), selection)
        } else {
            Occurrences::default()
        };
        if found == self.occurrences && self.occurrence_decorations.is_some() {
            return false;
        }
        let colors = MonacoColors::new(dark);
        let fill = |range: &Range<usize>, color| {
            RangeDecoration::new(range.clone())
                .with_style(RangeDecorationStyle::Fill)
                .with_color(color)
        };
        let decorations: Vec<RangeDecoration> = found
            .words
            .iter()
            .map(|range| fill(range, colors.word_highlight))
            .chain(
                found
                    .selections
                    .iter()
                    .map(|range| fill(range, colors.selection_highlight)),
            )
            .collect();
        self.occurrences = found;
        match &self.occurrence_decorations {
            Some(collection) => collection.set(decorations, cx),
            None => {
                self.occurrence_decorations = Some(editor.update(cx, |state, cx| {
                    state.create_range_decorations_collection(decorations, cx)
                }));
            }
        }
        true
    }

    /// The problem count for the status bar: (errors, warnings).
    pub fn problem_counts(&self) -> (usize, usize) {
        let errors = self
            .problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
            .count();
        (errors, self.problems.len() - errors)
    }

    /// Where the next problem after `offset` starts (`editor.action.marker.next`, F8), round
    /// to the first.
    pub fn next_problem(&self, offset: usize) -> Option<usize> {
        let mut starts: Vec<usize> = self.problems.iter().map(|p| p.range.start).collect();
        starts.sort_unstable();
        starts
            .iter()
            .find(|&&start| start > offset)
            .or(starts.first())
            .copied()
    }

    /// What the overview ruler and the line box draw for the editor's state now.
    pub fn overlay_data(&self, editor: &Entity<EditorState>, dark: bool, cx: &App) -> OverlayData {
        let state = editor.read(cx);
        let colors = MonacoColors::new(dark);
        let mut marks: Vec<Mark> = self
            .occurrences
            .words
            .iter()
            .chain(&self.occurrences.selections)
            .map(|range| Mark {
                lane: Lane::Center,
                lines: lines_of(&self.line_starts, range),
                color: colors.overview_ruler_occurrence,
            })
            .collect();
        marks.extend(self.problems.iter().map(|problem| Mark {
            lane: Lane::Right,
            lines: lines_of(&self.line_starts, &problem.range),
            color: match problem.severity {
                Severity::Error => colors.overview_ruler_error,
                Severity::Warning => colors.warning,
            },
        }));
        OverlayData {
            marks,
            cursor_line: state.cursor_position().line as usize,
            line_count: state.text().lines_len().max(1),
            selection_empty: state.selected_range().is_empty(),
            previewed_line: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_after_each_break() {
        assert_eq!(line_starts("a\nbc\n"), vec![0, 2, 5]);
        assert_eq!(line_starts(""), vec![0]);
    }
}
