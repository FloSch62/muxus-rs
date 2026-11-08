//! The language features Monaco gives the remote editor, kept per open file: detected links
//! (underlined; Ctrl/Cmd+hover and click open them), occurrence and selection highlights, and
//! problem markers (JSON's checks, the syntax errors of the tree-sitter parse) with the marker
//! navigation of F8. They feed the overview ruler, the minimap and the status bar's problem
//! count.

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

use super::json::json_problems;
use super::links::{DetectedLink, detect_links, link_at};
use super::markers::{Marker, MarkerNavigation, Severity, sort_markers};
use super::minimap::MinimapData;
use super::occurrences::{MAX_SEARCHED_BYTES, Occurrences, occurrences, word_at};
use super::overlays::{Lane, Mark, OverlayData, lines_of};
use super::syntax_problems::syntax_markers;
use crate::util::code_editor::SharedProbe;

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

/// Monaco's marker navigation widget, open under a problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemWidget {
    pub marker: Marker,
    /// The problem's number (from 1) and how many there are.
    pub number: usize,
    pub total: usize,
    /// Where it opened: the cursor, or the problem's start.
    pub position: usize,
    /// The problem is gone (the text changed under it).
    pub stale: bool,
}

/// One file's links, highlights and problems.
pub struct DocumentFeatures {
    links: Rc<RefCell<Vec<DetectedLink>>>,
    link_decorations: Option<TextDecorationCollection>,
    occurrence_decorations: Option<RangeDecorationCollection>,
    occurrences: Occurrences,
    /// The selection the highlights were found for.
    selection: Option<Range<usize>>,
    /// The problems, in the order F8 visits them.
    markers: Vec<Marker>,
    /// F8's place among the markers, and its widget while open.
    navigation: MarkerNavigation,
    widget: Option<ProblemWidget>,
    /// The editor's highlighter, and the version of its parse the markers were found in.
    probe: SharedProbe,
    analyzed_version: u64,
    /// Moves with every look at a changed text.
    revision: u64,
    line_starts: Vec<usize>,
    dark: bool,
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

impl DocumentFeatures {
    /// Hook the link opener into `editor`, whose highlighter reports to `probe`.
    pub fn install(editor: &Entity<EditorState>, probe: SharedProbe, cx: &mut App) -> Self {
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
            markers: Vec::new(),
            navigation: MarkerNavigation::default(),
            widget: None,
            probe,
            analyzed_version: 0,
            revision: 0,
            line_starts: vec![0],
            dark: false,
        }
    }

    /// The highlighter parsed again since the markers were found (it parses a newly set
    /// grammar when the editor next renders, after the text was looked at).
    pub fn probe_moved(&self) -> bool {
        self.probe.borrow().version() != self.analyzed_version
    }

    /// The probe of the editor's highlighter and the parse the markers come from.
    pub fn probe(&self) -> (SharedProbe, u64) {
        (self.probe.clone(), self.analyzed_version)
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
        self.revision += 1;
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
        let mut markers: Vec<Marker> = if language == "json" {
            if text.len() <= MAX_SEARCHED_BYTES {
                json_problems(&text).into_iter().map(Marker::from).collect()
            } else {
                Vec::new()
            }
        } else {
            let probe = self.probe.borrow();
            match probe.tree() {
                // The tree is the text's only when they are as long (the highlighter may not
                // have parsed a text just set).
                Some(tree) if tree.root_node().end_byte() == text.len() => {
                    syntax_markers(tree, &text, probe.grammar(), language)
                }
                _ => Vec::new(),
            }
        };
        self.analyzed_version = self.probe.borrow().version();
        sort_markers(&mut markers);
        if markers != self.markers {
            self.navigation.reset();
            // The widget follows the problem at its place, or shows the old one as stale.
            if let Some(widget) = &mut self.widget {
                match markers.iter().position(|marker| {
                    marker.range.start <= widget.position && widget.position <= marker.range.end
                }) {
                    Some(index) => {
                        widget.marker = markers[index].clone();
                        widget.number = index + 1;
                        widget.total = markers.len();
                        widget.stale = false;
                    }
                    None => widget.stale = true,
                }
            }
        }
        self.markers = markers;
        let diagnostics: Vec<Diagnostic> = self
            .markers
            .iter()
            .map(|marker| {
                let mut diagnostic = Diagnostic::new(
                    rope.offset_to_position(marker.range.start)
                        ..rope.offset_to_position(marker.range.end),
                    marker.message.clone(),
                )
                .with_severity(match marker.severity {
                    Severity::Error => DiagnosticSeverity::Error,
                    Severity::Warning => DiagnosticSeverity::Warning,
                });
                if let Some(source) = marker.source {
                    diagnostic = diagnostic.with_source(source);
                }
                if let Some(code) = &marker.code {
                    diagnostic = diagnostic.with_code(code.clone());
                }
                diagnostic
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
        self.navigation.follow_cursor(&self.markers, state.cursor());
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
            .markers
            .iter()
            .filter(|marker| marker.severity == Severity::Error)
            .count();
        (errors, self.markers.len() - errors)
    }

    /// The problems, in the order F8 visits them.
    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// F8 / Shift+F8 (`editor.action.marker.next`/`prev`): the next or previous problem from
    /// the one shown, or from the cursor at `offset` in `text`; its widget opens on it.
    /// Returns the problem.
    pub fn step_marker(&mut self, forward: bool, offset: usize, text: &str) -> Option<&Marker> {
        let index = self
            .navigation
            .step(&self.markers, forward, offset, |at| word_at(text, at))?;
        let marker = self.markers.get(index)?.clone();
        // The widget sits at the cursor when the cursor is in the problem, else at its start.
        let position = if marker.range.start <= offset && offset <= marker.range.end {
            offset
        } else {
            marker.range.start
        };
        self.widget = Some(ProblemWidget {
            marker,
            number: index + 1,
            total: self.markers.len(),
            position,
            stale: false,
        });
        self.markers.get(index)
    }

    /// The problem widget, while it is open.
    pub fn widget(&self) -> Option<&ProblemWidget> {
        self.widget.as_ref()
    }

    /// Escape, the widget's close button. Whether it was open.
    pub fn close_widget(&mut self) -> bool {
        self.widget.take().is_some()
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
        marks.extend(self.markers.iter().map(|marker| Mark {
            lane: Lane::Right,
            lines: lines_of(&self.line_starts, &marker.range),
            color: match marker.severity {
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
            probe: Some((self.probe.clone(), self.analyzed_version)),
            minimap_room: None,
        }
    }

    /// What the minimap draws: the text's colors, problems and highlights.
    pub fn minimap_data(&self, tab_size: usize) -> MinimapData {
        let grammar = self.probe.borrow().grammar().to_string();
        MinimapData {
            probe: self.probe.clone(),
            revision: self.revision,
            grammar,
            markers: self.markers.clone(),
            occurrences: self
                .occurrences
                .words
                .iter()
                .chain(&self.occurrences.selections)
                .cloned()
                .collect(),
            tab_size,
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
