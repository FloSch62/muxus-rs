//! The accents a command button can wear (`COMMAND_BUTTON_COLORS`): content
//! the user picks, not chrome, so they come from the stored color rather
//! than the theme. Each has a darker ink for light mode and a lighter one
//! for dark mode.

use muxus_store::prefs::command_buttons::CommandButtonColor;
use muxus_ui::color::hex;
use muxus_ui::gpui_kit::Hsla;
use muxus_ui::theme::Theme;

/// `commandButtonInk(color, mode)`.
pub fn command_button_ink(color: CommandButtonColor, theme: &Theme) -> Hsla {
    hex(color.ink(theme.mode.is_dark()))
}
