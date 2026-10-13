//! The window's dialogs that belong to feature screens (`App.tsx` mounts them beside the
//! shell): one slot per kind, each a GPUI entity drawn over the window and dropped when it
//! says it closed.

use muxus_ui::gpui_kit::{
    AnyView, Context, Entity, EntityId, EventEmitter, Render, Subscription, Window,
};

use crate::ui::shell::AppShell;

/// One mounted dialog.
pub struct Overlay {
    pub key: &'static str,
    pub view: AnyView,
    /// Modal dialogs take the keyboard: the window's shortcuts stand down while one is open.
    pub modal: bool,
    _subscription: Subscription,
}

impl AppShell {
    /// Mount `entity` as the dialog of kind `key` (replacing an open one of that kind);
    /// `on_event` handles what it emits and closes it with [`AppShell::close_overlay`].
    pub fn mount_overlay<V, E>(
        &mut self,
        key: &'static str,
        entity: Entity<V>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_event: impl Fn(&mut AppShell, &Entity<V>, &E, &mut Window, &mut Context<AppShell>) + 'static,
    ) where
        V: Render + EventEmitter<E>,
        E: 'static,
    {
        self.mount_layer(key, entity, true, window, cx, on_event);
    }

    /// Like [`AppShell::mount_overlay`], for layers that leave the window usable (menus that
    /// close on their own, progress, notices).
    pub fn mount_layer<V, E>(
        &mut self,
        key: &'static str,
        entity: Entity<V>,
        modal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_event: impl Fn(&mut AppShell, &Entity<V>, &E, &mut Window, &mut Context<AppShell>) + 'static,
    ) where
        V: Render + EventEmitter<E>,
        E: 'static,
    {
        self.overlays.retain(|overlay| overlay.key != key);
        let subscription = cx.subscribe_in(&entity, window, on_event);
        self.menu = None;
        self.overlays.push(Overlay {
            key,
            view: entity.into(),
            modal,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Drop the dialog of kind `key`; the keyboard goes back to the active terminal.
    ///
    /// The dialog goes once the events it emitted together with its `Close` are handled:
    /// dropping its subscription right away lost the rest (`Close` then `AddHost`).
    pub fn close_overlay(
        &mut self,
        key: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only the dialog open now: one of the same kind mounted meanwhile stays.
        let Some(view) = self
            .overlays
            .iter()
            .find(|overlay| overlay.key == key)
            .map(|overlay| overlay.view.entity_id())
        else {
            return;
        };
        cx.defer_in(window, move |this, window, cx| {
            this.remove_overlay(view, window, cx)
        });
    }

    fn remove_overlay(&mut self, view: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.overlays.len();
        self.overlays
            .retain(|overlay| overlay.view.entity_id() != view);
        if self.overlays.len() != before {
            // Another dialog underneath keeps the keyboard.
            if let Some(id) = self
                .tabs
                .active_id
                .clone()
                .filter(|_| self.overlays.is_empty())
            {
                self.focus_terminal(&id, window, cx);
            }
            cx.notify();
        }
    }

    /// Whether a modal feature dialog is open.
    pub fn modal_overlay_open(&self) -> bool {
        self.overlays.iter().any(|overlay| overlay.modal)
    }
}
