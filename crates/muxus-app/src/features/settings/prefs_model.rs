//! The preferences every window reads, with a change notification.
//!
//! In the TypeScript build each window holds the zustand prefs store and the
//! desktop shell keeps the windows' copies in step. Here every window runs in
//! one process, so one GPUI entity holds the preferences: [`SettingsPrefs`].
//! Views read it, change it with [`SettingsPrefs::update`], and subscribe to
//! [`PrefsChanged`] to apply a change live (theme, fonts, schemes, highlight
//! profiles in open terminals). Writes go to `client-state.json` through
//! [`Backend::set_prefs`], one at a time and always the newest state.
//!
//! What `App.tsx` does on every change is done here too: the application
//! theme follows `themeMode`, and the X11 switches and the debug log level
//! are sent to the backend.

use muxus_core::api::X11SettingsUpdate;
use muxus_session::Backend;
use muxus_store::prefs::PrefsState;
use muxus_store::prefs::state::ThemeMode;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, Global, WindowAppearance,
};
use muxus_ui::theme::{Mode, Theme};

/// The preferences changed. Both states are whole snapshots, so a listener
/// can check just the fields it cares about with [`PrefsChanged::changed`].
#[derive(Clone, Debug)]
pub struct PrefsChanged {
    pub previous: PrefsState,
    pub current: PrefsState,
}

impl PrefsChanged {
    /// Whether the part `field` picks out differs between the two states.
    pub fn changed<T: PartialEq>(&self, field: impl Fn(&PrefsState) -> T) -> bool {
        field(&self.previous) != field(&self.current)
    }
}

/// The shared preferences (see the module docs).
pub struct SettingsPrefs {
    backend: Backend,
    prefs: PrefsState,
    writing: bool,
    write_again: bool,
}

impl EventEmitter<PrefsChanged> for SettingsPrefs {}

struct GlobalPrefs(Entity<SettingsPrefs>);

impl Global for GlobalPrefs {}

/// Picks the X11 switches out of the preferences.
type X11Fields = fn(&PrefsState) -> (Option<bool>, Option<bool>, bool);

/// The light or dark appearance `mode` resolves to under the OS appearance.
pub fn effective_mode(mode: ThemeMode, appearance: WindowAppearance) -> Mode {
    match mode {
        ThemeMode::Light => Mode::Light,
        ThemeMode::Dark => Mode::Dark,
        ThemeMode::Os => match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Mode::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Mode::Light,
        },
    }
}

impl SettingsPrefs {
    /// Install the shared preferences (once, at startup, with what
    /// [`Backend::prefs`] returned). Calling it again replaces the state and
    /// notifies listeners, which is also how a restored backup is picked up.
    pub fn init(backend: Backend, prefs: PrefsState, cx: &mut App) -> Entity<SettingsPrefs> {
        if let Some(existing) = Self::global(cx) {
            existing.update(cx, |model, cx| {
                model.backend = backend;
                model.replace(prefs, cx);
            });
            return existing;
        }
        let entity = cx.new(|_| SettingsPrefs {
            backend,
            prefs,
            writing: false,
            write_again: false,
        });
        cx.set_global(GlobalPrefs(entity.clone()));
        entity.update(cx, |model, cx| model.apply_side_effects(None, cx));
        entity
    }

    /// The installed preferences entity, if [`SettingsPrefs::init`] ran.
    pub fn global(cx: &App) -> Option<Entity<SettingsPrefs>> {
        cx.try_global::<GlobalPrefs>()
            .map(|global| global.0.clone())
    }

    pub fn prefs(&self) -> &PrefsState {
        &self.prefs
    }

    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// Change the preferences (`set`): listeners hear about it at once and
    /// the new state is written in the background.
    pub fn update(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut PrefsState)) {
        let previous = self.prefs.clone();
        change(&mut self.prefs);
        if self.prefs == previous {
            return;
        }
        self.after_change(previous, cx);
        self.schedule_write(cx);
    }

    /// Take a state that is already stored (another window's write, a
    /// restored backup) without writing it again.
    pub fn replace(&mut self, prefs: PrefsState, cx: &mut Context<Self>) {
        if prefs == self.prefs {
            return;
        }
        let previous = std::mem::replace(&mut self.prefs, prefs);
        self.after_change(previous, cx);
    }

    /// Read the stored preferences again (after a restore wrote them).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(prefs) = backend.reload_prefs().await {
                let _ = this.update(cx, |model, cx| model.replace(prefs, cx));
            }
        })
        .detach();
    }

    fn after_change(&mut self, previous: PrefsState, cx: &mut Context<Self>) {
        self.apply_side_effects(Some(&previous), cx);
        cx.emit(PrefsChanged {
            previous,
            current: self.prefs.clone(),
        });
        cx.notify();
    }

    /// Install the theme the preferences ask for under the OS appearance.
    /// Windows call this again when the OS appearance changes.
    pub fn apply_theme(&self, cx: &mut App) {
        let mode = effective_mode(self.prefs.theme_mode, cx.window_appearance());
        if cx
            .try_global::<Theme>()
            .is_none_or(|theme| theme.mode != mode)
        {
            Theme::install(mode, cx);
        }
    }

    fn apply_side_effects(&mut self, previous: Option<&PrefsState>, cx: &mut Context<Self>) {
        let changed = |field: X11Fields| {
            previous.is_none_or(|previous| field(previous) != field(&self.prefs))
        };
        if previous.is_none_or(|previous| previous.theme_mode != self.prefs.theme_mode) {
            self.apply_theme(cx);
        }
        let backend = self.backend.clone();
        if changed(|p| {
            (
                p.x11_enabled,
                p.x11_forward_by_default,
                p.x11_clipboard_sharing,
            )
        }) {
            // Unset switches keep the backend's platform default.
            let update = X11SettingsUpdate {
                enabled: self.prefs.x11_enabled,
                forward_by_default: self.prefs.x11_forward_by_default,
                clipboard: self.prefs.x11_clipboard_sharing,
            };
            let backend = backend.clone();
            cx.background_spawn(async move {
                let _ = backend.put_x11_settings(update).await;
            })
            .detach();
        }
        if previous.is_none_or(|previous| previous.debug_mode != self.prefs.debug_mode) {
            let debug = self.prefs.debug_mode;
            cx.background_spawn(async move {
                let _ = backend.put_log_settings(debug).await;
            })
            .detach();
        }
    }

    fn schedule_write(&mut self, cx: &mut Context<Self>) {
        if self.writing {
            self.write_again = true;
            return;
        }
        self.writing = true;
        let backend = self.backend.clone();
        let first = self.prefs.clone();
        cx.spawn(async move |this, cx| {
            let mut snapshot = first;
            loop {
                if let Err(error) = backend.set_prefs(snapshot).await {
                    eprintln!("could not save the preferences: {error}");
                }
                let next = this
                    .update(cx, |model, _| {
                        if model.write_again {
                            model.write_again = false;
                            Some(model.prefs.clone())
                        } else {
                            model.writing = false;
                            None
                        }
                    })
                    .ok()
                    .flatten();
                match next {
                    Some(prefs) => snapshot = prefs,
                    None => break,
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_mode_follows_the_os() {
        assert_eq!(
            effective_mode(ThemeMode::Os, WindowAppearance::VibrantDark),
            Mode::Dark
        );
        assert_eq!(
            effective_mode(ThemeMode::Os, WindowAppearance::Light),
            Mode::Light
        );
        assert_eq!(
            effective_mode(ThemeMode::Dark, WindowAppearance::Light),
            Mode::Dark
        );
    }

    #[test]
    fn changes_report_the_fields_that_moved() {
        let previous = PrefsState::default();
        let mut current = previous.clone();
        current.mono_font_size = 16.;
        let change = PrefsChanged { previous, current };
        assert!(change.changed(|p| p.mono_font_size));
        assert!(!change.changed(|p| p.font_family.clone()));
    }
}
