//! Window placement files: `window-state.json` (the main window) and
//! `settings-window-state.json` (the settings window), from
//! `electron/src/main.ts` and `electron/src/window-placement.ts`.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use muxus_core::js;

/// A rectangle in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rectangle {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// Where a window was last left: its restored (not maximized) bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlacement {
    pub bounds: Rectangle,
    pub maximized: bool,
}

/// What the main window reopens with (`WindowState` in `main.ts`): a size,
/// and the position when one was saved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    pub width: f64,
    pub height: f64,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub maximized: bool,
}

/// The main window's size when nothing usable was saved.
pub const DEFAULT_WINDOW_STATE: WindowState = WindowState {
    width: 1440.0,
    height: 900.0,
    x: None,
    y: None,
    maximized: false,
};

/// The settings window's size when nothing usable was saved.
pub const SETTINGS_WINDOW_SIZE: Size = Size {
    width: 1000.0,
    height: 720.0,
};

fn finite(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
}

/// A stored placement, or `None` when the value holds anything else
/// (`parseWindowPlacement`).
pub fn parse_window_placement(value: &Value) -> Option<WindowPlacement> {
    let placement = value.as_object()?;
    let x = finite(placement.get("x"))?;
    let y = finite(placement.get("y"))?;
    let width = finite(placement.get("width"))?;
    let height = finite(placement.get("height"))?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(WindowPlacement {
        bounds: Rectangle {
            x,
            y,
            width,
            height,
        },
        maximized: placement.get("maximized") == Some(&Value::Bool(true)),
    })
}

/// The placement as `{ ...bounds, maximized }` JSON (`JSON.stringify`).
pub fn format_window_placement(placement: &WindowPlacement) -> String {
    let mut map = Map::new();
    map.insert("x".into(), js::js_number(placement.bounds.x));
    map.insert("y".into(), js::js_number(placement.bounds.y));
    map.insert("width".into(), js::js_number(placement.bounds.width));
    map.insert("height".into(), js::js_number(placement.bounds.height));
    map.insert("maximized".into(), Value::Bool(placement.maximized));
    Value::Object(map).to_string()
}

/// `loadWindowState`: the saved state when it has a numeric size, else the default.
pub fn load_window_state(path: &Path) -> WindowState {
    let Ok(text) = fs::read_to_string(path) else {
        return DEFAULT_WINDOW_STATE;
    };
    let Ok(Value::Object(state)) = serde_json::from_str::<Value>(&text) else {
        return DEFAULT_WINDOW_STATE;
    };
    let (Some(width), Some(height)) = (
        state.get("width").and_then(Value::as_f64),
        state.get("height").and_then(Value::as_f64),
    ) else {
        return DEFAULT_WINDOW_STATE;
    };
    WindowState {
        width,
        height,
        x: state.get("x").and_then(Value::as_f64),
        y: state.get("y").and_then(Value::as_f64),
        maximized: state.get("maximized") == Some(&Value::Bool(true)),
    }
}

/// `saveWindowState` / `saveSettingsWindowPlacement`: the normal bounds and
/// whether the window is maximized. Failures are the caller's to ignore; the
/// state is a nicety.
pub fn save_window_placement(path: &Path, placement: &WindowPlacement) -> io::Result<()> {
    fs::write(path, format_window_placement(placement))
}

/// `loadSettingsWindowPlacement`.
pub fn load_window_placement(path: &Path) -> Option<WindowPlacement> {
    let text = fs::read_to_string(path).ok()?;
    parse_window_placement(&serde_json::from_str(&text).ok()?)
}

/// Bounds for a window that remembers its placement, or only a size when the
/// system should place it (`windowBounds`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowBounds {
    Placed(Rectangle),
    SizeOnly(Size),
}

/// Displays can be unplugged or rearranged between runs, so saved bounds move
/// onto the display they overlap most and shrink to fit it. Without saved
/// bounds, or when they no longer touch any display, the window opens
/// centered over `owner`. Only the size comes back when neither is
/// available, and the system places it.
pub fn window_bounds(
    saved: Option<Rectangle>,
    owner: Option<Rectangle>,
    work_areas: &[Rectangle],
    default_size: Size,
) -> WindowBounds {
    if let Some(saved) = saved
        && let Some(area) = most_overlapping(saved, work_areas)
    {
        return WindowBounds::Placed(fit_inside(saved, area));
    }
    let size = saved.map_or(default_size, |saved| Size {
        width: saved.width,
        height: saved.height,
    });
    let Some(owner) = owner else {
        return WindowBounds::SizeOnly(size);
    };
    let area = most_overlapping(owner, work_areas);
    let centered = Rectangle {
        x: js_round(owner.x + (owner.width - size.width) / 2.0),
        y: js_round(owner.y + (owner.height - size.height) / 2.0),
        width: size.width,
        height: size.height,
    };
    WindowBounds::Placed(match area {
        Some(area) => fit_inside(centered, area),
        None => centered,
    })
}

fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn most_overlapping(bounds: Rectangle, work_areas: &[Rectangle]) -> Option<Rectangle> {
    let mut best = None;
    let mut best_overlap = 0.0;
    for area in work_areas {
        let overlap = overlap_area(bounds, *area);
        if overlap > best_overlap {
            best = Some(*area);
            best_overlap = overlap;
        }
    }
    best
}

fn overlap_area(a: Rectangle, b: Rectangle) -> f64 {
    let width = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let height = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    if width > 0.0 && height > 0.0 {
        width * height
    } else {
        0.0
    }
}

fn fit_inside(bounds: Rectangle, area: Rectangle) -> Rectangle {
    let width = bounds.width.min(area.width);
    let height = bounds.height.min(area.height);
    Rectangle {
        x: clamp(bounds.x, area.x, area.x + area.width - width),
        y: clamp(bounds.y, area.y, area.y + area.height - height),
        width,
        height,
    }
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    max.min(min.max(value))
}
