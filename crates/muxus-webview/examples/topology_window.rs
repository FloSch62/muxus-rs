//! Show a containerlab topology the way the app does, without the app:
//!
//! ```bash
//! cargo run -p muxus-webview --features webview --example topology_window -- \
//!     lab.clab.yml [light|dark] [seconds]
//! ```
//!
//! The example is its own window process (`--web-window`). Every node of the file is listed as
//! running (SSH as admin); the example prints the node the window asks to open, and closes the
//! window after `seconds` when given (after switching it to the other theme halfway).

use std::time::{Duration, Instant};

use muxus_webview::{
    TopologyNode, TopologyTheme, TopologyWindowRequest, WebWindowResult, WebWindows,
};

fn theme(mode: &str) -> TopologyTheme {
    // The app's MUI colors for each mode, as the app passes them.
    let dark = mode == "dark";
    let pick = |light: &str, dark_value: &str| if dark { dark_value } else { light }.to_owned();
    TopologyTheme {
        mode: mode.into(),
        background: pick("#f6f7f9", "#0f1115"),
        foreground: pick("#3f3f49", "#e6e6eb"),
        surface: pick("#ffffff", "#16181d"),
        border: pick("rgba(0, 0, 0, 0.12)", "rgba(255, 255, 255, 0.12)"),
        accent: pick("#1976d2", "#90caf9"),
        accent_text: pick("#ffffff", "rgba(0, 0, 0, 0.87)"),
        secondary: pick("rgba(0, 0, 0, 0.6)", "rgba(255, 255, 255, 0.7)"),
        disabled: pick("rgba(0, 0, 0, 0.38)", "rgba(255, 255, 255, 0.5)"),
        success: pick("#2e7d32", "#66bb6a"),
        disabled_background: pick("rgba(0, 0, 0, 0.12)", "rgba(255, 255, 255, 0.12)"),
        disabled_text: pick("rgba(0, 0, 0, 0.26)", "rgba(255, 255, 255, 0.3)"),
        warning_background: pick("#fff4e5", "#191207"),
        warning_text: pick("#663c00", "#ffe2b7"),
    }
}

/// The node names under `topology.nodes` of a block-style `.clab.yml`.
fn node_names(yaml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_nodes = None;
    for line in yaml.lines() {
        let depth = line.len() - line.trim_start().len();
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        match in_nodes {
            None if text == "nodes:" => in_nodes = Some((depth, None)),
            Some((nodes_depth, _)) if depth <= nodes_depth => in_nodes = None,
            Some((nodes_depth, ref mut node_depth)) => {
                let node_depth = *node_depth.get_or_insert(depth);
                if depth == node_depth && depth > nodes_depth {
                    names.push(text.trim_end_matches(':').trim_matches('"').to_owned());
                }
            }
            None => {}
        }
    }
    names
}

fn main() {
    if let Some(code) = muxus_webview::run_if_requested() {
        std::process::exit(code);
    }
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a .clab.yml file");
    let mode = args.next().unwrap_or_else(|| "light".into());
    let mut seconds: Option<u64> = args.next().and_then(|s| s.parse().ok());
    let yaml = std::fs::read_to_string(&path).expect("the topology file");
    let annotations =
        std::fs::read_to_string(path.replace(".clab.yml", ".clab.yml.annotations.json")).ok();
    let nodes = node_names(&yaml)
        .into_iter()
        .map(|name| TopologyNode {
            name,
            kind: "nokia_srlinux".into(),
            ipv4: Some("172.20.20.2".into()),
            running: true,
            ssh: true,
            disabled: false,
            reason: None,
        })
        .collect();
    let request = TopologyWindowRequest {
        key: "example-topology".into(),
        title: "example".into(),
        subtitle: format!("this computer · {path}"),
        yaml,
        annotations,
        theme: theme(&mode),
        nodes,
    };
    let data = std::env::temp_dir().join("muxus-topology-window-example");
    let windows = WebWindows::for_current_exe(&data).expect("the example's own path");
    match windows.open_topology(&request) {
        Ok(WebWindowResult::Opened) => println!("opened {path}"),
        other => {
            eprintln!("not opened: {other:?}");
            std::process::exit(1);
        }
    }
    let closed = windows.closed();
    let opened = windows.opened_nodes();
    let started = Instant::now();
    let mut switched = false;
    loop {
        if let Ok(node) = opened.try_recv() {
            println!("open node: {node:?}");
        }
        if let Ok(window) = closed.try_recv() {
            println!("closed: {window:?}");
            break;
        }
        if let Some(limit) = seconds {
            if !switched && started.elapsed() >= Duration::from_secs(limit / 2) {
                let other = if mode == "dark" { "light" } else { "dark" };
                println!("theme: {other}");
                windows.set_theme(&theme(other));
                switched = true;
            }
            if started.elapsed() >= Duration::from_secs(limit) {
                println!("closing after {}s", started.elapsed().as_secs());
                windows.close_all();
                seconds = None;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        if started.elapsed() > Duration::from_secs(600) {
            break;
        }
    }
}
