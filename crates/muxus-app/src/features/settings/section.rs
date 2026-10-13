//! The settings sections and where settings are opened to
//! (`SettingsSection` / `SettingsTarget` in `client/src/state/ui.ts`).

/// One page of the settings, in the order the section list shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SettingsSection {
    #[default]
    Appearance,
    Terminal,
    LocalShells,
    Logging,
    Highlighting,
    Behavior,
    X11,
    Keyboard,
    Passwords,
    Data,
    Debug,
    About,
}

impl SettingsSection {
    /// `SECTIONS`, in list order.
    pub const ALL: [SettingsSection; 12] = [
        SettingsSection::Appearance,
        SettingsSection::Terminal,
        SettingsSection::LocalShells,
        SettingsSection::Logging,
        SettingsSection::Highlighting,
        SettingsSection::Behavior,
        SettingsSection::X11,
        SettingsSection::Keyboard,
        SettingsSection::Passwords,
        SettingsSection::Data,
        SettingsSection::Debug,
        SettingsSection::About,
    ];

    /// The id the TypeScript uses (window launches, deep links).
    pub fn id(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "appearance",
            SettingsSection::Terminal => "terminal",
            SettingsSection::LocalShells => "local-shells",
            SettingsSection::Logging => "logging",
            SettingsSection::Highlighting => "highlighting",
            SettingsSection::Behavior => "behavior",
            SettingsSection::X11 => "x11",
            SettingsSection::Keyboard => "keyboard",
            SettingsSection::Passwords => "passwords",
            SettingsSection::Data => "data",
            SettingsSection::Debug => "debug",
            SettingsSection::About => "about",
        }
    }

    /// `isSettingsSection`.
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Terminal => "Terminal",
            SettingsSection::LocalShells => "Local shells",
            SettingsSection::Logging => "Session logging",
            SettingsSection::Highlighting => "Highlighting",
            SettingsSection::Behavior => "Behavior",
            SettingsSection::X11 => "X11 forwarding",
            SettingsSection::Keyboard => "Keyboard",
            SettingsSection::Passwords => "Passwords",
            SettingsSection::Data => "Backup & data",
            SettingsSection::Debug => "Debug",
            SettingsSection::About => "About",
        }
    }

    /// The Material icon beside the label.
    pub fn icon(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "PaletteOutlined",
            SettingsSection::Terminal => "Terminal",
            SettingsSection::LocalShells => "CodeOutlined",
            SettingsSection::Logging => "HistoryOutlined",
            SettingsSection::Highlighting => "HighlightOutlined",
            SettingsSection::Behavior => "TuneOutlined",
            SettingsSection::X11 => "DesktopWindowsOutlined",
            SettingsSection::Keyboard => "KeyboardOutlined",
            SettingsSection::Passwords => "PasswordOutlined",
            SettingsSection::Data => "BackupOutlined",
            SettingsSection::Debug => "BugReportOutlined",
            SettingsSection::About => "InfoOutlined",
        }
    }
}

/// A section, and optionally one entry in it, to bring into view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsTarget {
    pub section: SettingsSection,
    /// The entry settings were opened for (a local shell profile id),
    /// highlighted until another section is picked.
    pub item: Option<String>,
}

impl SettingsTarget {
    pub fn section(section: SettingsSection) -> Self {
        Self {
            section,
            item: None,
        }
    }

    /// `settingsTarget` of `SettingsWindow.tsx`: a launch's section and item,
    /// or nothing when the section is unknown.
    pub fn from_launch(section: Option<&str>, item: Option<&str>) -> Option<Self> {
        Some(Self {
            section: SettingsSection::parse(section?)?,
            item: item.map(str::to_string),
        })
    }
}

/// What a change of section asks for (`useSettingsNavigation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Navigation {
    /// Switch now.
    Go(SettingsTarget),
    /// Same section: only the highlighted entry changes.
    Focus(Option<String>),
    /// Unsaved session logging edits: ask before leaving.
    Confirm(SettingsTarget),
}

/// Where settings stand: the open section, the entry they were opened for,
/// and whether the logging section holds unsaved edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsNavigation {
    pub section: SettingsSection,
    pub focus_item: Option<String>,
    pub logging_dirty: bool,
}

impl SettingsNavigation {
    pub fn new(initial: Option<SettingsTarget>) -> Self {
        let initial = initial.unwrap_or_default();
        Self {
            section: initial.section,
            focus_item: initial.item,
            logging_dirty: false,
        }
    }

    /// Whether leaving the current section would drop unsaved logging edits.
    pub fn holds_unsaved_logging(&self) -> bool {
        self.logging_dirty && self.section == SettingsSection::Logging
    }

    /// `show`: bring a section (and entry) into view, unless that would
    /// silently drop unsaved logging edits.
    pub fn show(&mut self, target: SettingsTarget) -> Navigation {
        if target.section == self.section {
            self.focus_item = target.item.clone();
            return Navigation::Focus(target.item);
        }
        if self.holds_unsaved_logging() {
            return Navigation::Confirm(target);
        }
        self.apply(target.clone());
        Navigation::Go(target)
    }

    /// `pick`: a click in the section list.
    pub fn pick(&mut self, section: SettingsSection) -> Option<Navigation> {
        (section != self.section).then(|| self.show(SettingsTarget::section(section)))
    }

    /// The user agreed to discard the logging edits.
    pub fn discard_and_go(&mut self, target: SettingsTarget) {
        self.logging_dirty = false;
        self.apply(target);
    }

    fn apply(&mut self, target: SettingsTarget) {
        self.section = target.section;
        self.focus_item = target.item;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_round_trip_their_ids() {
        for section in SettingsSection::ALL {
            assert_eq!(SettingsSection::parse(section.id()), Some(section));
        }
        assert_eq!(SettingsSection::parse("nope"), None);
        assert_eq!(
            SettingsTarget::from_launch(Some("local-shells"), Some("local-1")),
            Some(SettingsTarget {
                section: SettingsSection::LocalShells,
                item: Some("local-1".into())
            })
        );
        assert_eq!(SettingsTarget::from_launch(None, Some("x")), None);
    }

    #[test]
    fn leaving_unsaved_logging_edits_asks_first() {
        let mut nav =
            SettingsNavigation::new(Some(SettingsTarget::section(SettingsSection::Logging)));
        nav.logging_dirty = true;
        let target = SettingsTarget::section(SettingsSection::Terminal);
        assert_eq!(
            nav.show(target.clone()),
            Navigation::Confirm(target.clone())
        );
        assert_eq!(nav.section, SettingsSection::Logging);
        // The same section only moves the highlighted entry.
        assert_eq!(
            nav.show(SettingsTarget {
                section: SettingsSection::Logging,
                item: Some("x".into())
            }),
            Navigation::Focus(Some("x".into()))
        );
        nav.discard_and_go(target);
        assert_eq!(nav.section, SettingsSection::Terminal);
        assert!(!nav.logging_dirty);
        assert_eq!(nav.pick(SettingsSection::Terminal), None);
        assert_eq!(
            nav.pick(SettingsSection::About),
            Some(Navigation::Go(SettingsTarget::section(
                SettingsSection::About
            )))
        );
    }

    #[test]
    fn dirty_flags_outside_logging_never_hold_navigation() {
        let mut nav = SettingsNavigation::new(None);
        nav.logging_dirty = true;
        assert!(matches!(
            nav.pick(SettingsSection::Keyboard),
            Some(Navigation::Go(_))
        ));
    }
}
