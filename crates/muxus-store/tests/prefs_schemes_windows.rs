//! Port of the pure parts of `tests/unit/client/custom-terminal-schemes.test.ts`
//! (turning schemes into terminal themes belongs to the terminal view) and of
//! `tests/unit/electron/window-placement.test.ts`.

use muxus_store::prefs::custom_schemes::*;
use muxus_store::prefs::window_state::*;
use serde_json::{Map, Value, json};

fn frappe_colors() -> Value {
    json!({
        "background": "#303446", "foreground": "#c6d0f5", "cursor": "#f2d5cf", "selectionBackground": "#626880",
        "black": "#51576d", "red": "#e78284", "green": "#a6d189", "yellow": "#e5c890",
        "blue": "#8caaee", "magenta": "#f4b8e4", "cyan": "#81c8be", "white": "#b5bfe2",
        "brightBlack": "#626880", "brightRed": "#e78284", "brightGreen": "#a6d189", "brightYellow": "#e5c890",
        "brightBlue": "#8caaee", "brightMagenta": "#f4b8e4", "brightCyan": "#81c8be", "brightWhite": "#a5adce",
    })
}

fn frappe_value() -> Value {
    json!({ "id": "scheme-frappe", "name": "Catppuccin Frappé", "colors": frappe_colors() })
}

fn frappe() -> CustomTerminalScheme {
    serde_json::from_value(frappe_value()).unwrap()
}

/// Catppuccin Frappé as its Windows Terminal port ships it.
fn windows_terminal_frappe() -> Value {
    json!({
        "name": "Catppuccin Frappe", "cursorColor": "#F2D5CF", "selectionBackground": "#626880",
        "background": "#303446", "foreground": "#C6D0F5", "black": "#51576D", "red": "#E78284",
        "green": "#A6D189", "yellow": "#E5C890", "blue": "#8CAAEE", "purple": "#F4B8E4", "cyan": "#81C8BE",
        "white": "#B5BFE2", "brightBlack": "#626880", "brightRed": "#E78284", "brightGreen": "#A6D189",
        "brightYellow": "#E5C890", "brightBlue": "#8CAAEE", "brightPurple": "#F4B8E4", "brightCyan": "#81C8BE",
        "brightWhite": "#A5ADCE",
    })
}

const ITERM_HEX: [&str; 16] = [
    "#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#b5bfe2",
    "#626880", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#a5adce",
];

fn iterm_names() -> Vec<(String, &'static str)> {
    let mut names = vec![
        ("Background Color".to_owned(), "#303446"),
        ("Foreground Color".to_owned(), "#c6d0f5"),
        ("Cursor Color".to_owned(), "#f2d5cf"),
        ("Selection Color".to_owned(), "#626880"),
    ];
    names.extend(
        ITERM_HEX
            .iter()
            .enumerate()
            .map(|(index, color)| (format!("Ansi {index} Color"), *color)),
    );
    names
}

fn iterm_color(name: &str, hex: &str, alpha: f64) -> String {
    let component = |offset: usize| {
        f64::from(u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap()) / 255.0
    };
    format!(
        "\t<key>{name}</key>\n\t<dict>\n\t\t<key>Alpha Component</key>\n\t\t<real>{alpha}</real>\n\t\t<key>Blue Component</key>\n\t\t<real>{}</real>\n\t\t<key>Color Space</key>\n\t\t<string>sRGB</string>\n\t\t<key>Green Component</key>\n\t\t<real>{}</real>\n\t\t<key>Red Component</key>\n\t\t<real>{}</real>\n\t</dict>",
        component(5),
        component(3),
        component(1)
    )
}

fn iterm_preset(colors: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n{}\n</dict>\n</plist>\n",
        colors.join("\n")
    )
}

#[test]
fn accepts_a_complete_scheme_and_rejects_malformed_ones() {
    assert!(is_custom_terminal_scheme_array(&json!([])));
    assert!(is_custom_terminal_scheme_array(&json!([frappe_value()])));
    assert!(!is_custom_terminal_scheme_array(&json!({})));
    assert!(!is_custom_terminal_scheme_array(&json!([
        frappe_value(),
        frappe_value()
    ])));
    let with = |key: &str, value: Value| {
        let mut scheme = frappe_value();
        scheme[key] = value;
        json!([scheme])
    };
    assert!(!is_custom_terminal_scheme_array(&with("name", json!("  "))));
    assert!(!is_custom_terminal_scheme_array(&with(
        "id",
        json!("x".repeat(65))
    )));
    let mut red = frappe_colors();
    red["red"] = json!("red");
    assert!(!is_custom_terminal_scheme_array(&with("colors", red)));
    let mut without_cursor = frappe_colors();
    without_cursor
        .as_object_mut()
        .unwrap()
        .shift_remove("cursor");
    assert!(!is_custom_terminal_scheme_array(&with(
        "colors",
        without_cursor
    )));
    let mut extra = frappe_colors();
    extra["extra"] = json!("#000000");
    assert!(!is_custom_terminal_scheme_array(&with("colors", extra)));
    let too_many: Vec<Value> = (0..=MAX_CUSTOM_TERMINAL_SCHEMES)
        .map(|index| {
            let mut scheme = frappe_value();
            scheme["id"] = json!(format!("scheme-{index}"));
            scheme
        })
        .collect();
    assert!(!is_custom_terminal_scheme_array(&Value::Array(too_many)));
    // A custom scheme never takes a built-in id.
    assert!(!is_custom_terminal_scheme_array(&with(
        "id",
        json!("dracula")
    )));
}

#[test]
fn classifies_a_scheme_by_its_background() {
    assert!(!is_light_color(&frappe().colors.background));
    assert!(is_light_color("#ffffff"));
    assert!(is_light_color("#eff1f5"));
    assert!(!is_light_color("#1e1e2e"));
}

#[test]
fn copies_a_scheme_under_a_free_name_flattening_translucent_colors() {
    // Catppuccin Mocha's palette, its selection translucent as the built-in theme has it.
    let mut mocha: Map<String, Value> = serde_json::from_value(frappe_colors()).unwrap();
    mocha.insert("background".into(), json!("#1e1e2e"));
    mocha.insert("magenta".into(), json!("#f5c2e7"));
    mocha.insert(
        "selectionBackground".into(),
        json!("rgba(88, 91, 112, 0.6)"),
    );
    let copy = custom_scheme_copy_of("Catppuccin Mocha", &mocha, &[]).unwrap();
    assert_eq!(copy.name, "Catppuccin Mocha copy");
    assert_eq!(copy.colors.background, "#1e1e2e");
    assert_eq!(copy.colors.magenta, "#f5c2e7");
    assert_eq!(copy.colors.selection_background, "#414356");
    assert!(are_valid_custom_schemes(std::slice::from_ref(&copy)));
    let second =
        custom_scheme_copy_of("Catppuccin Mocha", &mocha, std::slice::from_ref(&copy)).unwrap();
    assert_eq!(second.name, "Catppuccin Mocha copy 2");
    assert_ne!(second.id, copy.id);
}

#[test]
fn round_trips_an_export() {
    let document = create_terminal_scheme_document(&[frappe()]).unwrap();
    assert_eq!(document.format, TERMINAL_SCHEME_FILE_FORMAT);
    assert_eq!(
        parse_terminal_scheme_file(&serde_json::to_string(&document).unwrap(), "export.json")
            .unwrap(),
        vec![frappe()]
    );
    assert!(
        create_terminal_scheme_document(&[])
            .unwrap_err()
            .contains("at least one")
    );
}

#[test]
fn rejects_an_export_from_a_newer_version_or_with_invalid_schemes() {
    let document =
        serde_json::to_value(create_terminal_scheme_document(&[frappe()]).unwrap()).unwrap();
    let mut newer = document.clone();
    newer["version"] = json!(2);
    assert!(
        parse_terminal_scheme_file(&newer.to_string(), "export.json")
            .unwrap_err()
            .contains("version 2 is not supported")
    );
    let mut builtin = document;
    builtin["schemes"][0]["id"] = json!("nord");
    assert!(
        parse_terminal_scheme_file(&builtin.to_string(), "export.json")
            .unwrap_err()
            .contains("incomplete or invalid")
    );
}

#[test]
fn imports_a_windows_terminal_color_scheme() {
    let schemes =
        parse_terminal_scheme_file(&windows_terminal_frappe().to_string(), "frappe.json").unwrap();
    assert_eq!(schemes[0].name, "Catppuccin Frappe");
    assert_eq!(
        serde_json::to_value(&schemes[0].colors).unwrap(),
        frappe_colors()
    );
    assert!(are_valid_custom_schemes(&schemes));
}

#[test]
fn imports_every_scheme_of_a_windows_terminal_settings_file() {
    let mut nameless = windows_terminal_frappe();
    nameless.as_object_mut().unwrap().shift_remove("name");
    let schemes = parse_terminal_scheme_file(
        &json!({ "schemes": [windows_terminal_frappe(), nameless] }).to_string(),
        "settings.json",
    )
    .unwrap();
    assert_eq!(
        schemes
            .iter()
            .map(|scheme| scheme.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Catppuccin Frappe", "settings 2"]
    );
    assert_ne!(schemes[0].id, schemes[1].id);
}

#[test]
fn derives_the_cursor_and_selection_when_left_out() {
    let mut bare = windows_terminal_frappe();
    bare.as_object_mut().unwrap().shift_remove("cursorColor");
    bare.as_object_mut()
        .unwrap()
        .shift_remove("selectionBackground");
    let scheme = &parse_terminal_scheme_file(&bare.to_string(), "frappe.json").unwrap()[0];
    assert_eq!(scheme.colors.cursor, "#c6d0f5");
    // The text color at 30% over the background.
    assert_eq!(scheme.colors.selection_background, "#5d637b");
}

#[test]
fn reads_short_hex_and_rgb_notations() {
    let mut source = windows_terminal_frappe();
    source["black"] = json!("#000");
    source["white"] = json!("rgb(255, 255, 255)");
    source["selectionBackground"] = json!("rgba(255, 255, 255, 0.5)");
    let scheme = &parse_terminal_scheme_file(&source.to_string(), "scheme.json").unwrap()[0];
    assert_eq!(scheme.colors.black, "#000000");
    assert_eq!(scheme.colors.white, "#ffffff");
    assert_eq!(scheme.colors.selection_background, "#989aa3");
}

#[test]
fn names_the_color_a_json_scheme_is_missing() {
    let mut incomplete = windows_terminal_frappe();
    incomplete.as_object_mut().unwrap().shift_remove("purple");
    assert!(
        parse_terminal_scheme_file(&incomplete.to_string(), "frappe.json")
            .unwrap_err()
            .contains("Catppuccin Frappe: the color \"magenta (or purple)\" is missing")
    );
    assert!(
        parse_terminal_scheme_file("{\"format\":\"muxus-backup\"}", "backup.json")
            .unwrap_err()
            .contains("backup: the color \"background\" is missing")
    );
}

#[test]
fn imports_an_iterm2_preset_named_after_the_file() {
    let colors: Vec<String> = iterm_names()
        .iter()
        .map(|(name, color)| iterm_color(name, color, 1.0))
        .collect();
    let scheme =
        &parse_terminal_scheme_file(&iterm_preset(&colors), "Catppuccin Frappe.itermcolors")
            .unwrap()[0];
    assert_eq!(scheme.name, "Catppuccin Frappe");
    assert_eq!(
        serde_json::to_value(&scheme.colors).unwrap(),
        frappe_colors()
    );
}

#[test]
fn flattens_a_translucent_iterm2_selection_and_reports_a_missing_color() {
    let colors: Vec<String> = iterm_names()
        .iter()
        .map(|(name, color)| {
            iterm_color(
                name,
                color,
                if name == "Selection Color" { 0.5 } else { 1.0 },
            )
        })
        .collect();
    let scheme =
        &parse_terminal_scheme_file(&iterm_preset(&colors), "half.itermcolors").unwrap()[0];
    assert_eq!(scheme.colors.selection_background, "#494e63");
    let without_red: Vec<String> = colors
        .into_iter()
        .filter(|color| !color.contains("<key>Ansi 1 Color</key>"))
        .collect();
    assert!(
        parse_terminal_scheme_file(&iterm_preset(&without_red), "half.itermcolors")
            .unwrap_err()
            .contains("half: the color \"red\" is missing")
    );
}

#[test]
fn rejects_files_that_are_not_a_color_scheme() {
    assert!(
        parse_terminal_scheme_file("not json", "notes.txt")
            .unwrap_err()
            .contains("neither")
    );
    assert!(
        parse_terminal_scheme_file("<html></html>", "page.html")
            .unwrap_err()
            .contains("neither")
    );
    assert!(
        parse_terminal_scheme_file("[]", "empty.json")
            .unwrap_err()
            .contains("no color schemes")
    );
    assert!(
        parse_terminal_scheme_file("[42]", "numbers.json")
            .unwrap_err()
            .contains("does not describe")
    );
}

#[test]
fn merging_replaces_known_ids_in_place_and_names_new_schemes_freely() {
    let other = CustomTerminalScheme {
        id: "scheme-other".into(),
        name: "Other".into(),
        ..frappe()
    };
    let updated = CustomTerminalScheme {
        name: "Frappé, retuned".into(),
        ..frappe()
    };
    assert_eq!(
        merge_custom_terminal_schemes(&[frappe(), other.clone()], std::slice::from_ref(&updated))
            .unwrap(),
        vec![updated, other]
    );
    let again = CustomTerminalScheme {
        id: "scheme-again".into(),
        ..frappe()
    };
    let dracula = CustomTerminalScheme {
        id: "scheme-dracula".into(),
        name: "Dracula".into(),
        ..frappe()
    };
    let merged = merge_custom_terminal_schemes(&[frappe()], &[again, dracula]).unwrap();
    assert_eq!(
        merged
            .iter()
            .map(|scheme| scheme.name.as_str())
            .collect::<Vec<_>>(),
        // The built-in scheme already uses the plain name.
        vec!["Catppuccin Frappé", "Catppuccin Frappé 2", "Dracula 2"]
    );
    assert!(are_valid_custom_schemes(&merged));
    let full: Vec<CustomTerminalScheme> = (0..MAX_CUSTOM_TERMINAL_SCHEMES)
        .map(|index| CustomTerminalScheme {
            id: format!("scheme-{index}"),
            name: format!("Scheme {index}"),
            ..frappe()
        })
        .collect();
    assert!(
        merge_custom_terminal_schemes(&full, &[frappe()])
            .unwrap_err()
            .contains("exceed the limit")
    );
}

const LAPTOP: Rectangle = Rectangle {
    x: 0.0,
    y: 0.0,
    width: 1920.0,
    height: 1080.0,
};
const MONITOR: Rectangle = Rectangle {
    x: 1920.0,
    y: -200.0,
    width: 2560.0,
    height: 1400.0,
};
const SIZE: Size = Size {
    width: 1000.0,
    height: 720.0,
};

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rectangle {
    Rectangle {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn settings_window_reopens_where_it_was_left() {
    let saved = rect(2400.0, 100.0, 900.0, 700.0);
    assert_eq!(
        window_bounds(Some(saved), Some(LAPTOP), &[LAPTOP, MONITOR], SIZE),
        WindowBounds::Placed(saved)
    );
    assert_eq!(
        window_bounds(
            Some(rect(1300.0, 50.0, 1000.0, 700.0)),
            Some(LAPTOP),
            &[LAPTOP, MONITOR],
            SIZE
        ),
        WindowBounds::Placed(rect(920.0, 50.0, 1000.0, 700.0))
    );
    assert_eq!(
        window_bounds(
            Some(rect(100.0, 100.0, 2400.0, 1300.0)),
            None,
            &[LAPTOP],
            SIZE
        ),
        WindowBounds::Placed(rect(0.0, 0.0, 1920.0, 1080.0))
    );
}

#[test]
fn settings_window_centers_over_the_app_window_when_it_cannot_reopen() {
    let owner = rect(100.0, 100.0, 1400.0, 900.0);
    assert_eq!(
        window_bounds(
            Some(rect(2400.0, 100.0, 900.0, 700.0)),
            Some(owner),
            &[LAPTOP],
            SIZE
        ),
        WindowBounds::Placed(rect(350.0, 200.0, 900.0, 700.0))
    );
    assert_eq!(
        window_bounds(
            None,
            Some(rect(2000.0, 0.0, 1600.0, 1000.0)),
            &[LAPTOP, MONITOR],
            SIZE
        ),
        WindowBounds::Placed(rect(2300.0, 140.0, 1000.0, 720.0))
    );
    assert_eq!(
        window_bounds(
            None,
            Some(rect(-300.0, 900.0, 800.0, 600.0)),
            &[LAPTOP],
            SIZE
        ),
        WindowBounds::Placed(rect(0.0, 360.0, 1000.0, 720.0))
    );
    assert_eq!(
        window_bounds(None, None, &[LAPTOP], SIZE),
        WindowBounds::SizeOnly(SIZE)
    );
}

#[test]
fn reads_only_well_formed_stored_placements() {
    let placement = parse_window_placement(
        &json!({ "x": 10, "y": 20, "width": 900, "height": 700, "maximized": true }),
    )
    .unwrap();
    assert_eq!(
        placement,
        WindowPlacement {
            bounds: rect(10.0, 20.0, 900.0, 700.0),
            maximized: true
        }
    );
    let not_max = parse_window_placement(
        &json!({ "x": 10, "y": 20, "width": 900, "height": 700, "maximized": "yes" }),
    )
    .unwrap();
    assert!(!not_max.maximized);
    assert_eq!(
        parse_window_placement(&json!({ "x": 10, "y": 20, "width": 0, "height": 700 })),
        None
    );
    assert_eq!(
        parse_window_placement(&json!({ "x": "10", "y": 20, "width": 900, "height": 700 })),
        None
    );
    assert_eq!(parse_window_placement(&json!([10, 20, 900, 700])), None);
    assert_eq!(parse_window_placement(&Value::Null), None);
}

#[test]
fn window_state_files_round_trip_like_electron() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("window-state.json");
    assert_eq!(load_window_state(&path), DEFAULT_WINDOW_STATE);
    let placement = WindowPlacement {
        bounds: rect(12.0, 34.0, 1280.0, 800.0),
        maximized: false,
    };
    save_window_placement(&path, &placement).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"x":12,"y":34,"width":1280,"height":800,"maximized":false}"#
    );
    let state = load_window_state(&path);
    assert_eq!(
        (state.width, state.height, state.x, state.y, state.maximized),
        (1280.0, 800.0, Some(12.0), Some(34.0), false)
    );
    assert_eq!(load_window_placement(&path), Some(placement));
    std::fs::write(&path, r#"{"width":"wide"}"#).unwrap();
    assert_eq!(load_window_state(&path), DEFAULT_WINDOW_STATE);
}
