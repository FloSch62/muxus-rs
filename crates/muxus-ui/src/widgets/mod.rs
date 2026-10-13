//! MUI components as Muxus uses them, drawn with plain GPUI elements so the
//! geometry is exactly MUI's under the Muxus theme.

pub mod button;
pub mod controls;
pub mod dialog;
pub mod display;
pub mod form_control;
pub mod form_field;
pub mod hover;
pub mod layer;
pub mod link;
pub mod list;
pub mod listbox;
pub mod menu;
pub mod progress;
pub mod scrollbar;
pub mod search_field;
pub mod select;
pub mod tabs;
pub mod text_field;
pub mod toggle_button;
pub mod tooltip;
pub mod tooltip_lines;
pub mod typography;
pub mod window_frame;

pub use button::{Button, ButtonColor, ButtonVariant, IconButton};
pub use controls::{Checkbox, Switch};
pub use dialog::{Dialog, DialogWidth};
pub use display::{Alert, Chip};
pub use menu::MenuItem;
pub use select::Select;
pub use tabs::Tab;
pub use text_field::TextField;
pub use tooltip::{AnchorBounds, Placement, Tooltip, bounds_probe};

use gpui_kit::App;

pub fn init(_cx: &mut App) {}
