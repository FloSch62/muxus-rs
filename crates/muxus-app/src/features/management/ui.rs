//! The workbench's view state: what React kept in component state (widths,
//! filters, open menus and dialogs, result trees per run). The workbench's
//! own state (drafts, runs, explorer) lives in `WorkbenchStore`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use muxus_core::management::SavedManagementRequest;
use muxus_mgmt::workbench::data_tree::DataNode;
use muxus_ui::gpui_kit::{FocusHandle, Pixels, Point, UniformListScrollHandle};
use muxus_ui::widgets::tooltip::AnchorBounds;

use super::fields::FieldKey;
use super::logic::diff::DiffRow;
use super::logic::editor::Confirmation;
use super::logic::live::Sort;

pub const NAVIGATOR_DEFAULT_WIDTH: f32 = 300.;
pub const NAVIGATOR_MIN_WIDTH: f32 = 220.;
/// Below this pane width the navigator floats over the work area instead of sitting beside it.
pub const NARROW_WIDTH: f32 = 760.;

/// Which tree a node menu or row action belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeSource {
    Explorer,
    /// The result tree of a run.
    Result(String),
}

/// An open popup menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Menu {
    /// Right-click menu of a data node.
    Node {
        source: TreeSource,
        node_id: String,
        position: Point<Pixels>,
    },
    CopyAs(Point<Pixels>),
    NetconfOperation(Point<Pixels>),
    /// A select field's options.
    Select {
        field: SelectField,
        position: Point<Pixels>,
        width: f32,
    },
}

/// The select fields of the forms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelectField {
    Encoding,
    SetOp(String),
    SetEncoding(String),
    WithDefaults,
    DefaultOperation,
    TestOption,
    ErrorOption,
    /// A select of a gNOI/gNSI tool.
    Tool(&'static str),
    /// A select in a row of a tool's list, by position.
    ToolRow(&'static str, usize),
}

/// One diff of the review dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewSection {
    pub label: String,
    pub original: String,
    pub modified: String,
    /// `json` or `xml`.
    pub language: &'static str,
    /// Shown above the diff, e.g. "Not configured yet".
    pub note: Option<String>,
    pub delta: (usize, usize),
    pub rows: Vec<DiffRow>,
}

impl ReviewSection {
    pub fn new(
        label: String,
        original: String,
        modified: String,
        language: &'static str,
        note: Option<String>,
    ) -> Self {
        let delta = super::logic::diff::line_delta(&original, &modified);
        let rows = super::logic::diff::diff_rows(&original, &modified);
        Self {
            label,
            original,
            modified,
            language,
            note,
            delta,
            rows,
        }
    }
}

/// What the review dialog's apply button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewApply {
    /// Run the editor's Set.
    RunDraft,
    /// Commit the candidate.
    Commit,
    /// Start the rotation a security tool prepared (`authz`, `pathz`).
    Rotate(&'static str),
}

/// What a confirmation leads to.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    /// Run the editor's draft as it is now.
    RunDraft,
    /// Run a NETCONF discard-changes.
    Discard,
    /// Delete a saved request.
    DeleteRequest(String),
    /// What a tool asked about.
    Tool(super::tools::ToolAction),
}

/// What a text prompt leads to.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptAction {
    RenameRequest(SavedManagementRequest),
    CommitConfirmed,
    /// Add a TLS profile (certz).
    AddCertzProfile,
}

/// An open dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    Review {
        title: String,
        description: Option<String>,
        sections: Vec<ReviewSection>,
        loading: bool,
        error: Option<String>,
        apply: Option<(ReviewApply, String)>,
        tab: usize,
    },
    Save {
        host_only: bool,
        saving: bool,
    },
    Confirm {
        confirmation: Confirmation,
        action: ConfirmAction,
    },
    Prompt {
        title: String,
        description: Option<String>,
        label: String,
        confirm_label: String,
        action: PromptAction,
        error: Option<String>,
    },
    /// `TypedConfirmDialog`: the device name must be typed before a disruptive operation.
    TypedConfirm {
        title: String,
        description: String,
        expected: String,
        confirm_label: String,
        action: super::tools::ToolAction,
    },
    /// A device file shown read-only (the Files tool's preview).
    FilePreview {
        path: String,
        text: Option<String>,
        loading: bool,
        error: Option<String>,
    },
}

/// The saved request library, as last fetched.
#[derive(Debug, Clone, Default)]
pub struct Library {
    pub requests: Vec<SavedManagementRequest>,
    pub loading: bool,
    pub loaded: bool,
}

/// A run's result as a tree, with its own expansion (`useTreeState`).
#[derive(Debug, Clone)]
pub struct ResultTree {
    pub root: DataNode,
    pub expanded: HashSet<String>,
    pub selected: Option<String>,
}

/// The live view's display state.
#[derive(Debug, Clone, Default)]
pub struct LiveUi {
    pub paused: bool,
    pub sort: Sort,
}

/// A drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Navigator { start_x: f32, start_width: f32 },
    Split,
}

/// The path field whose suggestions are open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathSuggest {
    pub field: Option<FieldKey>,
    pub open: bool,
    pub highlighted: usize,
}

pub struct Ui {
    /// The pane's width as last laid out.
    pub width: Rc<Cell<f32>>,
    pub was_narrow: bool,
    pub navigator_width: f32,
    /// `None`: the request area fits its content; a share once the divider was dragged.
    pub request_share: Option<f32>,
    pub main: AnchorBounds,
    pub drag: Option<Drag>,
    pub explorer_filter: String,
    pub library: Library,
    pub library_filter: String,
    pub device_filter: String,
    pub notification_open: Option<usize>,
    pub results: HashMap<String, ResultTree>,
    pub result_filter: String,
    pub live: LiveUi,
    pub live_filter: String,
    /// What a paused live display shows: rows, leaf updates, bytes, start, synced.
    pub live_frozen: Option<(
        Vec<muxus_mgmt::workbench::live_table::LiveRow>,
        u64,
        u64,
        u64,
        bool,
    )>,
    /// The value a text prompt opens with.
    pub prompt_initial: Option<String>,
    /// Focus the path field just added.
    pub focus_new_path: bool,
    /// Explorer levels a path field already asked for.
    pub path_loads: HashSet<String>,
    /// The gNMI form's Advanced section; `None` follows the draft (open when it has a prefix).
    pub advanced: Option<bool>,
    pub preview: bool,
    pub set_loading: Option<String>,
    pub suggest: PathSuggest,
    pub menu: Option<Menu>,
    pub dialog: Option<Dialog>,
    pub anchors: HashMap<String, AnchorBounds>,
    pub explorer_scroll: UniformListScrollHandle,
    pub result_scroll: UniformListScrollHandle,
    pub table_scroll: UniformListScrollHandle,
    pub live_scroll: UniformListScrollHandle,
    pub focus: HashMap<String, FocusHandle>,
    pub tools: super::tools::ToolsUi,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            width: Rc::new(Cell::new(1200.)),
            was_narrow: false,
            navigator_width: NAVIGATOR_DEFAULT_WIDTH,
            request_share: None,
            main: AnchorBounds::default(),
            drag: None,
            explorer_filter: String::new(),
            library: Library::default(),
            library_filter: String::new(),
            device_filter: String::new(),
            notification_open: None,
            results: HashMap::new(),
            result_filter: String::new(),
            live: LiveUi::default(),
            live_filter: String::new(),
            live_frozen: None,
            prompt_initial: None,
            focus_new_path: false,
            path_loads: HashSet::new(),
            advanced: None,
            preview: false,
            set_loading: None,
            suggest: PathSuggest::default(),
            menu: None,
            dialog: None,
            anchors: HashMap::new(),
            explorer_scroll: UniformListScrollHandle::new(),
            result_scroll: UniformListScrollHandle::new(),
            table_scroll: UniformListScrollHandle::new(),
            live_scroll: UniformListScrollHandle::new(),
            focus: HashMap::new(),
            tools: Default::default(),
        }
    }
}

impl Ui {
    /// A slot a control records its painted bounds in, by name.
    pub fn anchor(&mut self, name: impl Into<String>) -> AnchorBounds {
        self.anchors.entry(name.into()).or_default().clone()
    }

    pub fn narrow(&self) -> bool {
        self.width.get() < NARROW_WIDTH
    }
}

/// `clampNavigator`: at least the minimum, at most 60% of the pane and 640 px.
pub fn clamp_navigator(width: f32, container: f32) -> f32 {
    let upper = (container * 0.6).clamp(NAVIGATOR_MIN_WIDTH, 640.);
    width.clamp(NAVIGATOR_MIN_WIDTH, upper).round()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_the_navigator() {
        assert_eq!(clamp_navigator(100., 1200.), 220.);
        assert_eq!(clamp_navigator(900., 1200.), 640.);
        assert_eq!(clamp_navigator(900., 800.), 480.);
        assert_eq!(clamp_navigator(300., 300.), 220.);
        assert_eq!(clamp_navigator(333.4, 1200.), 333.);
    }
}
