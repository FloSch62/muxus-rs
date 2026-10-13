//! A local shell in a terminal view: `cargo run -p muxus-term-view --example shell [-- program args…]`.
//!
//! The shell runs on a pty with a scratch `HOME` (`MUXUS_TERM_HOME`, default a fresh directory
//! under the system temp dir) and without `ZDOTDIR`, so it never reads the real rc files. Fonts are
//! loaded from `MUXUS_TERM_FONTS` (a directory of `.ttf`/`.otf` files, e.g. JetBrains Mono and the
//! Pure Nerd Font symbols); the app registers its bundled fonts itself.
//!
//! Environment knobs for screenshots: `MUXUS_TERM_LINE_HEIGHT` (the capture harness uses 1.1),
//! `MUXUS_TERM_SCHEME` (a scheme id), `MUXUS_TERM_APP_LIGHT` (light app chrome),
//! `MUXUS_TERM_SEARCH` (open find with this query).

use std::io::{Read, Write};
use std::path::PathBuf;

use gpui_kit::*;
use muxus_core::stream::{TransportChannels, TransportEvent, TransportInput};
use muxus_term_view::muxus_term::palette::terminal_scheme;
use muxus_term_view::muxus_term::search::SearchOptions;
use muxus_term_view::{
    ClearScrollback, Copy, Find, Paste, TerminalSettings, TerminalView, TerminalViewEvent, ZoomIn,
    ZoomOut, ZoomReset, new_terminal_view,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn scratch_home() -> PathBuf {
    let home = std::env::var_os("MUXUS_TERM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("muxus-term-shell-{}", std::process::id()))
        });
    std::fs::create_dir_all(&home).expect("create scratch home");
    home
}

/// Start the program on a pty and connect it to a fresh pair of transport channels.
fn spawn_pty(program: Vec<String>) -> TransportChannels {
    let (channels, ends) = TransportChannels::pair(256);
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");
    let home = scratch_home();
    let mut command = if program.is_empty() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut command = CommandBuilder::new(shell);
        command.arg("-l");
        command
    } else {
        let mut command = CommandBuilder::new(&program[0]);
        command.args(&program[1..]);
        command
    };
    command.env("HOME", &home);
    command.env_remove("ZDOTDIR");
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.cwd(&home);
    let mut child = pty.slave.spawn_command(command).expect("spawn shell");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("pty reader");
    let events = ends.events.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if events
                        .send_blocking(TransportEvent::Data(buffer[..read].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let events = ends.events.clone();
    std::thread::spawn(move || {
        let status = child.wait().ok();
        let code = status.map(|s| s.exit_code() as i32);
        let _ = events.send_blocking(TransportEvent::Exit { code, signal: None });
    });
    let mut writer = pty.master.take_writer().expect("pty writer");
    let master = pty.master;
    std::thread::spawn(move || {
        while let Ok(message) = ends.input.recv_blocking() {
            match message {
                TransportInput::Data(data) => {
                    if writer.write_all(&data).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
                TransportInput::Resize(size) => {
                    let _ = master.resize(PtySize {
                        rows: size.rows,
                        cols: size.cols,
                        pixel_width: size.pixel_width,
                        pixel_height: size.pixel_height,
                    });
                }
                TransportInput::Break { .. } => {}
                TransportInput::Close => break,
            }
        }
    });
    channels
}

fn load_fonts(cx: &mut App) {
    let Some(dir) = std::env::var_os("MUXUS_TERM_FONTS") else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let fonts: Vec<std::borrow::Cow<'static, [u8]>> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "ttf" || ext == "otf")
        })
        .filter_map(|path| std::fs::read(path).ok())
        .map(std::borrow::Cow::Owned)
        .collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("could not load fonts: {error}");
    }
}

struct Shell {
    terminal: Entity<TerminalView>,
    _subscription: Subscription,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let light = std::env::var_os("MUXUS_TERM_APP_LIGHT").is_some();
        div()
            .size_full()
            .bg(rgb(if light { 0xffffff } else { 0x1b1b1f }))
            .child(self.terminal.clone())
    }
}

fn main() {
    let program: Vec<String> = std::env::args().skip(1).collect();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            load_fonts(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-shift-c", Copy, Some("Terminal")),
                KeyBinding::new("ctrl-shift-v", Paste, Some("Terminal")),
                KeyBinding::new("ctrl-shift-f", Find, Some("Terminal")),
                KeyBinding::new("ctrl-shift-k", ClearScrollback, Some("Terminal")),
                KeyBinding::new("ctrl-=", ZoomIn, Some("Terminal")),
                KeyBinding::new("ctrl--", ZoomOut, Some("Terminal")),
                KeyBinding::new("ctrl-0", ZoomReset, Some("Terminal")),
            ]);
            let mut settings = TerminalSettings::default();
            if let Some(height) = std::env::var("MUXUS_TERM_LINE_HEIGHT")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                settings.line_height = height;
            }
            if let Ok(id) = std::env::var("MUXUS_TERM_SCHEME") {
                let scheme = terminal_scheme(Some(&id), &[]);
                settings.theme = scheme.theme.clone();
                settings.theme_is_light = scheme.light;
            }
            settings.file_links = true;
            if std::env::var_os("MUXUS_TERM_APP_LIGHT").is_some() {
                settings.app_dark = false;
            }
            let search = std::env::var("MUXUS_TERM_SEARCH").ok();
            let bounds = Bounds::centered(None, size(px(1420.0), px(880.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            };
            let program = program.clone();
            gpui_kit::open_window(options, cx, move |window, cx| {
                let channels = spawn_pty(program);
                let terminal = new_terminal_view(channels, settings, window, cx);
                let subscription = cx.subscribe(
                    &terminal,
                    |terminal, event: &TerminalViewEvent, cx| match event {
                        TerminalViewEvent::OpenUrl(url) => cx.open_url(url),
                        TerminalViewEvent::Exited { code, .. } => {
                            let notice = format!(
                                "\r\n\x1b[90m[process exited with code {}]\x1b[0m\r\n",
                                code.unwrap_or(-1)
                            );
                            terminal
                                .update(cx, |view, cx| view.write_output(notice.as_bytes(), cx));
                        }
                        TerminalViewEvent::Failed(message) => {
                            eprintln!("transport failed: {message}")
                        }
                        _ => {}
                    },
                );
                terminal.update(cx, |view, cx| {
                    window.focus(view.focus_handle(), cx);
                    if let Some(term) = search {
                        cx.spawn_in(window, async move |this, cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(2500))
                                .await;
                            let _ = this.update_in(cx, |view, window, cx| {
                                view.set_search_query(&term, SearchOptions::default(), window, cx)
                            });
                        })
                        .detach();
                    }
                });
                cx.new(|_| Shell {
                    terminal,
                    _subscription: subscription,
                })
            })
            .expect("open window");
        });
}
