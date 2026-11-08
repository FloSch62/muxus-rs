//! The hosts sidebar in a headless window: dragging hosts and folders
//! (`useTreeDnd.ts`), the tree's keyboard (`useTreeKeyboard.ts`, Alt+Arrow
//! reordering) and the selection bar, driven with simulated input.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext};
use muxus_app::model::{Catalog, HostMetadata, ResolvedHostSettings, SshHostEntry};
use muxus_app::sidebar::folder_prefs::FolderPrefs;
use muxus_app::sidebar::row_style::{TREE_ROW_HEIGHT, TREE_ROW_PITCH};
use muxus_ui::gpui_kit::{Bounds, Entity, Pixels, Point, point, px};

use super::sidebar::{SessionSidebar, SidebarAction, SidebarEvent};
use super::sidebar_dnd::{FolderDrop, HostDrop};

const CONFIG: &str = "/home/test/.ssh/config";

fn host(alias: &str, group: Option<&str>) -> SshHostEntry {
    SshHostEntry {
        alias: alias.into(),
        aliases: vec![alias.into()],
        file: CONFIG.into(),
        resolved: ResolvedHostSettings {
            hostname: format!("{alias}.example.test"),
            port: 22,
            ..Default::default()
        },
        metadata: group.map(|group| HostMetadata {
            profile_id: format!("ssh-{alias}"),
            group: Some(group.into()),
            ..Default::default()
        }),
        description: None,
        options: Default::default(),
    }
}

/// Lab (gamma), Prod (alpha, beta) and delta outside every folder.
fn catalog() -> Catalog {
    let mut catalog = Catalog::default();
    catalog.ssh.path = CONFIG.into();
    catalog.ssh.files = vec![CONFIG.into()];
    catalog.ssh.hosts = vec![
        host("alpha", Some("Prod")),
        host("beta", Some("Prod")),
        host("gamma", Some("Lab")),
        host("delta", None),
    ];
    catalog
}

struct Harness<'a> {
    sidebar: Entity<SessionSidebar>,
    cx: &'a mut VisualTestContext,
    events: Rc<RefCell<Vec<SidebarEvent>>>,
}

fn harness(cx: &mut TestAppContext) -> Harness<'_> {
    cx.update(|cx| muxus_ui::init(muxus_ui::theme::Mode::Light, cx));
    let (sidebar, cx) = cx.add_window_view(|window, cx| {
        SessionSidebar::new(catalog(), FolderPrefs::default(), window, cx)
    });
    let events: Rc<RefCell<Vec<SidebarEvent>>> = Rc::default();
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&sidebar, move |_, event: &SidebarEvent, _| {
            sink.borrow_mut().push(event.clone())
        })
        .detach()
    });
    let mut harness = Harness {
        sidebar,
        cx,
        events,
    };
    harness.redraw();
    harness
}

impl Harness<'_> {
    fn redraw(&mut self) {
        self.cx.update(|window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn tree(&mut self) -> Bounds<Pixels> {
        self.cx
            .debug_bounds("host-tree")
            .expect("the tree is painted")
    }

    fn index_of(&mut self, label: &str) -> usize {
        self.sidebar.read_with(self.cx, |sidebar, _| {
            sidebar
                .rows
                .iter()
                .position(|row| row.node.label() == label)
                .unwrap_or_else(|| panic!("no row {label}"))
        })
    }

    fn key_of(&mut self, label: &str) -> String {
        let index = self.index_of(label);
        self.sidebar
            .read_with(self.cx, |sidebar, _| sidebar.rows[index].key.clone())
    }

    /// A point `ratio` down the row labelled `label`.
    fn row_point(&mut self, label: &str, ratio: f32) -> Point<Pixels> {
        let index = self.index_of(label);
        let tree = self.tree();
        point(
            tree.origin.x + px(80.),
            tree.origin.y + px(index as f32 * TREE_ROW_PITCH + TREE_ROW_HEIGHT * ratio),
        )
    }

    /// Press on `from`, move past the drag threshold, then over `to` and let go there.
    fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>) {
        self.cx
            .simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        self.cx.simulate_mouse_move(
            from + point(px(0.), px(6.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        self.redraw();
        self.cx
            .simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
        self.redraw();
        self.cx
            .simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
        self.cx
            .simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        self.redraw();
    }

    fn actions(&mut self) -> Vec<SidebarAction> {
        self.events
            .borrow_mut()
            .drain(..)
            .filter_map(|event| match event {
                SidebarEvent::Action(action) => Some(action),
                _ => None,
            })
            .collect()
    }

    fn focused(&mut self) -> Option<String> {
        self.sidebar
            .read_with(self.cx, |sidebar, _| sidebar.focused_key.clone())
    }
}

/// The drag was never wired: rows only reordered from their menus. A host dragged onto a
/// folder's middle moves into it.
#[gpui::test]
fn a_host_dragged_onto_a_folder_moves_into_it(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    let from = h.row_point("delta", 0.5);
    let to = h.row_point("Prod", 0.5);
    h.drag(from, to);
    let delta = h.key_of("delta");
    match h.actions().as_slice() {
        [
            SidebarAction::DropHosts(HostDrop {
                host_keys, path, ..
            }),
        ] => {
            assert_eq!(host_keys, &[delta]);
            assert_eq!(path.as_deref(), Some("Prod"));
        }
        other => panic!("unexpected {other:?}"),
    }
    // Nothing of the drag stays behind.
    h.sidebar.read_with(h.cx, |sidebar, _| {
        assert!(sidebar.drag.source.is_none() && sidebar.drag.target.is_none());
    });
}

/// Grabbing one host of a selection drags the whole selection; dropping on a host's
/// lower half places them after it.
#[gpui::test]
fn a_selection_is_dragged_together(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    for label in ["alpha", "beta"] {
        let at = h.row_point(label, 0.5);
        h.cx.simulate_click(at, Modifiers::control());
    }
    let (alpha, beta, gamma) = (h.key_of("alpha"), h.key_of("beta"), h.key_of("gamma"));
    h.sidebar.read_with(h.cx, |sidebar, _| {
        assert_eq!(sidebar.selection.len(), 2);
    });
    let from = h.row_point("alpha", 0.5);
    let to = h.row_point("gamma", 0.75);
    h.drag(from, to);
    match h.actions().as_slice() {
        [
            SidebarAction::DropHosts(HostDrop {
                host_keys,
                path,
                order,
            }),
        ] => {
            assert_eq!(host_keys, &[alpha.clone(), beta.clone()]);
            assert_eq!(path.as_deref(), Some("Lab"));
            assert_eq!(order, &[gamma, alpha, beta]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Past the last row is "no folder": the root target shows while dragging and takes the drop.
#[gpui::test]
fn dropping_below_the_rows_leaves_every_folder(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    let from = h.row_point("gamma", 0.5);
    h.cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(
        from + point(px(0.), px(6.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    h.redraw();
    let rows = h.sidebar.read_with(h.cx, |sidebar, _| sidebar.rows.len());
    let tree = h.tree();
    // The "Drop here for no folder" row sits 4px under the last row.
    let below = point(
        tree.origin.x + px(80.),
        tree.origin.y + px(rows as f32 * TREE_ROW_PITCH + 12.),
    );
    h.cx.simulate_mouse_move(below, MouseButton::Left, Modifiers::none());
    h.redraw();
    h.sidebar.read_with(h.cx, |sidebar, _| {
        assert!(sidebar.drag.active());
        assert_eq!(
            sidebar.drag.target,
            Some(muxus_app::sidebar::tree_dnd::DropTarget::Root)
        );
    });
    h.cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::none());
    h.redraw();
    match h.actions().as_slice() {
        [SidebarAction::DropHosts(HostDrop { path, .. })] => {
            assert_eq!(path.as_deref(), Some(""))
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// A folder dropped on another folder's top quarter takes its place among the siblings.
#[gpui::test]
fn a_folder_dropped_beside_another_is_placed_before_it(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    let (lab, prod) = (h.key_of("Lab"), h.key_of("Prod"));
    let from = h.row_point("Prod", 0.5);
    let to = h.row_point("Lab", 0.1);
    h.drag(from, to);
    match h.actions().as_slice() {
        [
            SidebarAction::DropFolder(FolderDrop {
                from,
                to,
                placement: Some((_, order)),
            }),
        ] => {
            assert_eq!((from.as_str(), to.as_str()), ("Prod", "Prod"));
            assert_eq!(order, &[prod, lab]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Hovering a collapsed folder opens it after a moment, and it closes again when the drag ends.
#[gpui::test]
fn a_collapsed_folder_opens_while_hovered_in_a_drag(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    let lab = h.key_of("Lab");
    h.sidebar.update(h.cx, |sidebar, cx| {
        sidebar.folders.set_collapsed(&lab, true);
        sidebar.rebuild();
        cx.notify();
    });
    h.redraw();
    let visible = |h: &mut Harness| {
        h.sidebar.read_with(h.cx, |sidebar, _| {
            sidebar.rows.iter().any(|row| row.node.label() == "gamma")
        })
    };
    assert!(!visible(&mut h));
    let from = h.row_point("delta", 0.5);
    h.cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(
        from + point(px(0.), px(6.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    h.redraw();
    let over = h.row_point("Lab", 0.5);
    h.cx.simulate_mouse_move(over, MouseButton::Left, Modifiers::none());
    h.cx.executor()
        .advance_clock(std::time::Duration::from_millis(600));
    h.redraw();
    assert!(visible(&mut h), "the folder opened under the pointer");
    h.cx.simulate_mouse_up(
        point(px(5000.), px(5000.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    h.redraw();
    assert!(!visible(&mut h), "it closes again after the drag");
}

/// Type-ahead was missing: printable keys jump to the next row whose label starts with what
/// was typed.
#[gpui::test]
fn typing_at_the_tree_jumps_to_a_matching_row(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    h.sidebar.update_in(h.cx, |sidebar, window, cx| {
        sidebar.focus_index(0, window, cx)
    });
    h.cx.simulate_keystrokes("g");
    let gamma = h.key_of("gamma");
    assert_eq!(h.focused(), Some(gamma.clone()));
    // A second letter extends the search rather than starting over.
    h.cx.simulate_keystrokes("a");
    assert_eq!(h.focused(), Some(gamma));
    h.cx.simulate_keystrokes("x");
    assert_eq!(
        h.focused(),
        Some(h.key_of("gamma")),
        "no match leaves focus alone"
    );
}

/// Alt+Arrow reorders the focused row: a host asks the window to write the new order, a
/// folder changes the folder order.
#[gpui::test]
fn alt_arrows_reorder_the_focused_row(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    let alpha = h.index_of("alpha");
    h.sidebar.update_in(h.cx, |sidebar, window, cx| {
        sidebar.focus_index(alpha, window, cx)
    });
    h.cx.simulate_keystrokes("alt-down");
    let key = h.key_of("alpha");
    match h.actions().as_slice() {
        [SidebarAction::MoveHost { key: moved, delta }] => {
            assert_eq!((moved, *delta), (&key, 1));
        }
        other => panic!("unexpected {other:?}"),
    }
    let lab = h.index_of("Lab");
    h.sidebar.update_in(h.cx, |sidebar, window, cx| {
        sidebar.focus_index(lab, window, cx)
    });
    h.cx.simulate_keystrokes("alt-down");
    assert!(
        h.index_of("Lab") > h.index_of("Prod"),
        "Lab moved below Prod"
    );
    // Plain arrows still move the focus.
    h.cx.simulate_keystrokes("up");
    assert_ne!(h.focused(), Some(h.key_of("Lab")));
}

/// Shift+Space toggles a host into the selection; the selection bar acts on all of them and
/// Escape lets go.
#[gpui::test]
fn the_selection_bar_follows_the_selection(cx: &mut TestAppContext) {
    let mut h = harness(cx);
    let alpha = h.index_of("alpha");
    h.sidebar.update_in(h.cx, |sidebar, window, cx| {
        sidebar.focus_index(alpha, window, cx)
    });
    h.cx.simulate_keystrokes("shift-space down shift-space");
    h.redraw();
    let selected = h
        .sidebar
        .read_with(h.cx, |sidebar, _| sidebar.selected_hosts().len());
    assert_eq!(selected, 2);
    assert!(h.actions().is_empty(), "selecting opens nothing");
    h.cx.simulate_keystrokes("escape");
    let selected = h
        .sidebar
        .read_with(h.cx, |sidebar, _| sidebar.selection.len());
    assert_eq!(selected, 0);
}
