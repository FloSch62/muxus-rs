//! What the window does with WebKitGTK directly: its keyboard shortcuts, its right-click menu,
//! the question about an untrusted certificate (`handleWebCertificateError`), and the windows
//! pages open.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use gtk::gdk::ModifierType;
use gtk::gdk::keys::constants as key;
use gtk::gio::prelude::TlsCertificateExt;
use gtk::gio::{SimpleAction, TlsCertificate, TlsCertificateFlags};
use gtk::glib::{self, Propagation};
use gtk::prelude::*;
use sha2::{Digest, Sha256};
use webkit2gtk::{
    ContextMenu, ContextMenuExt, ContextMenuItem, HitTestResult, HitTestResultExt, WebContextExt,
    WebView, WebViewExt,
};
use wry::{WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

thread_local! {
    /// The views of the windows pages opened, until their windows close.
    static PAGE_WINDOWS: RefCell<HashMap<u64, wry::WebView>> = RefCell::default();
    static NEXT_PAGE_WINDOW: Cell<u64> = const { Cell::new(0) };
    /// Certificates being asked about, as `host fingerprint`.
    static ASKING: RefCell<HashSet<String>> = RefCell::default();
}

/// The shortcuts, the right-click menu and the certificate question, for a page view of this
/// window.
pub(super) fn attach(webview: &WebView) {
    webview.connect_key_press_event(|webview, event| {
        if shortcut(webview, event) {
            Propagation::Stop
        } else {
            Propagation::Proceed
        }
    });
    webview.connect_context_menu(|webview, menu, _, hit| {
        context_menu(webview, menu, hit);
        // Show the menu, with our items in place of WebKit's.
        false
    });
    webview.connect_load_failed_with_tls_errors(|webview, uri, certificate, errors| {
        ask_about_certificate(webview, uri, certificate, errors);
        true
    });
}

/// What the page's edit state allows, as Chromium's `editFlags` put it: cut a selection in a
/// field, copy any selection, paste into a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditFlags {
    can_cut: bool,
    can_copy: bool,
    can_paste: bool,
}

impl EditFlags {
    fn new(editable: bool, selection: bool) -> Self {
        Self {
            can_cut: editable && selection,
            can_copy: selection,
            can_paste: editable,
        }
    }
}

/// One item of the menu: its label, whether it is enabled, and what it does.
type MenuEntry = (&'static str, bool, fn(&WebView));

/// The six items of the window's menu (`context-menu` in `web-windows.ts`), `None` for the
/// separator: Back and Forward while there is history that way, Reload, then Cut, Copy and
/// Paste as the page's edit state allows.
fn menu_entries(back: bool, forward: bool, edit: EditFlags) -> [Option<MenuEntry>; 7] {
    [
        Some(("Back", back, |view| view.go_back())),
        Some(("Forward", forward, |view| view.go_forward())),
        Some(("Reload", true, |view| view.reload())),
        None,
        Some(("Cut", edit.can_cut, |view| {
            view.execute_editing_command("Cut")
        })),
        Some(("Copy", edit.can_copy, |view| {
            view.execute_editing_command("Copy")
        })),
        Some(("Paste", edit.can_paste, |view| {
            view.execute_editing_command("Paste")
        })),
    ]
}

/// Replace WebKit's own menu (with its link, image and input method items) by the six items
/// the Electron window shows.
fn context_menu(webview: &WebView, menu: &ContextMenu, hit: &HitTestResult) {
    let edit = EditFlags::new(hit.context_is_editable(), hit.context_is_selection());
    menu.remove_all();
    for (index, entry) in menu_entries(webview.can_go_back(), webview.can_go_forward(), edit)
        .into_iter()
        .enumerate()
    {
        let Some((label, enabled, run)) = entry else {
            menu.append(&ContextMenuItem::new_separator());
            continue;
        };
        // WebKit puts the menu's actions in a group of their own, so the names only need to
        // differ within this menu.
        let action = SimpleAction::new(&format!("muxus-menu-{index}"), None);
        action.set_enabled(enabled);
        let view = webview.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(view) = view.upgrade() {
                run(&view);
            }
        });
        menu.append(&ContextMenuItem::from_gaction(&action, label, None));
    }
}

/// Alt+Left / Ctrl+[ back, Alt+Right / Ctrl+] forward, F5 / Ctrl+R reload.
fn shortcut(webview: &WebView, event: &gtk::gdk::EventKey) -> bool {
    let state = event.state();
    let alt = state.contains(ModifierType::MOD1_MASK);
    let ctrl = state.contains(ModifierType::CONTROL_MASK);
    let keyval = event.keyval();
    if (alt && keyval == key::Left) || (ctrl && keyval == key::bracketleft) {
        if webview.can_go_back() {
            webview.go_back();
        }
    } else if (alt && keyval == key::Right) || (ctrl && keyval == key::bracketright) {
        if webview.can_go_forward() {
            webview.go_forward();
        }
    } else if keyval == key::F5 || (ctrl && (keyval == key::r || keyval == key::R)) {
        webview.reload();
    } else {
        return false;
    }
    true
}

/// `SHA-256 AB:CD:…` of the certificate as sent.
fn fingerprint(certificate: &TlsCertificate) -> String {
    let der = certificate
        .certificate()
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    let hex: Vec<String> = Sha256::digest(&der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    format!("SHA-256 {}", hex.join(":"))
}

/// Why the certificate is not trusted, one reason per line.
fn describe(errors: TlsCertificateFlags) -> String {
    let reasons = [
        (
            TlsCertificateFlags::UNKNOWN_CA,
            "It is not signed by an authority this computer trusts.",
        ),
        (
            TlsCertificateFlags::BAD_IDENTITY,
            "It was not issued for this address.",
        ),
        (TlsCertificateFlags::NOT_ACTIVATED, "It is not valid yet."),
        (TlsCertificateFlags::EXPIRED, "It has expired."),
        (TlsCertificateFlags::REVOKED, "It has been revoked."),
        (
            TlsCertificateFlags::INSECURE,
            "It uses an insecure algorithm.",
        ),
    ];
    let lines: Vec<&str> = reasons
        .iter()
        .filter(|(flag, _)| errors.contains(*flag))
        .map(|(_, reason)| *reason)
        .collect();
    if lines.is_empty() {
        "It could not be checked.".to_owned()
    } else {
        lines.join("\n")
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// What the window shows when the certificate was not accepted and there is no page to go
/// back to.
fn refused_page(host: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"light dark\">\
         <title>Certificate not trusted</title>\
         <body style=\"font: 15px system-ui, sans-serif; margin: 4em auto; max-width: 36em\">\
         <h2>Certificate not trusted</h2>\
         <p>The certificate of {} is not trusted, so the page was not opened.</p>\
         <p>Reload (F5) to be asked again.</p></body>",
        escape_html(host)
    )
}

/// Internal web consoles often use self-signed certificates. Ask once per certificate instead
/// of refusing outright, the way a browser's "proceed anyway" does. A yes trusts the
/// certificate for its host in this window's session only, until the window closes.
fn ask_about_certificate(
    webview: &WebView,
    uri: &str,
    certificate: &TlsCertificate,
    errors: TlsCertificateFlags,
) {
    let parsed = url::Url::parse(uri).ok();
    let host_name = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or(uri)
        .to_owned();
    // `new URL(url).host`: the name, with a port that is not the scheme's own.
    let host = match parsed.as_ref().and_then(url::Url::port) {
        Some(port) => format!("{host_name}:{port}"),
        None => host_name.clone(),
    };
    let fingerprint = fingerprint(certificate);
    let key = format!("{host} {fingerprint}");
    // A page that fails again while the question is open asks once, not twice.
    if !ASKING.with(|asking| asking.borrow_mut().insert(key.clone())) {
        return;
    }
    let parent = webview
        .toplevel()
        .and_then(|widget| widget.downcast::<gtk::Window>().ok());
    let dialog = gtk::MessageDialog::new(
        parent.as_ref(),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        gtk::MessageType::Warning,
        gtk::ButtonsType::None,
        &format!("The certificate of {host} is not trusted."),
    );
    dialog.set_title("Certificate not trusted");
    dialog.set_secondary_text(Some(&format!(
        "{}\n\n{fingerprint}\n\nContinue only if you expect this certificate, such as a device's self-signed one.",
        describe(errors)
    )));
    dialog.add_button("Go back", gtk::ResponseType::Cancel);
    dialog.add_button("Continue anyway", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let (webview, certificate, uri) = (webview.clone(), certificate.clone(), uri.to_owned());
    let answered = Cell::new(false);
    dialog.connect_response(move |dialog, response| {
        // Closing the dialog answers it once more (`DeleteEvent`); the first answer counts.
        if answered.replace(true) {
            return;
        }
        dialog.close();
        ASKING.with(|asking| asking.borrow_mut().remove(&key));
        if response == gtk::ResponseType::Accept {
            if let Some(context) = webview.context() {
                context.allow_tls_certificate_for_host(&certificate, &host_name);
            }
            webview.load_uri(&uri);
        } else if webview.can_go_back() {
            webview.go_back();
        } else {
            webview.load_alternate_html(&refused_page(&host), &uri, None);
        }
    });
    dialog.show_all();
}

/// A window for a page a page opened: related to its opener, so it shares the host's session
/// and proxy, with the same rules. It belongs to the host window's process and goes with it.
pub(super) fn open_page_window(url: &str, opener: &WebView) -> Option<WebView> {
    let application = opener
        .toplevel()?
        .downcast::<gtk::ApplicationWindow>()
        .ok()?
        .application()?;
    let window = gtk::ApplicationWindow::builder()
        .application(&application)
        .title(url)
        .default_width(1000)
        .default_height(700)
        .build();
    window.set_icon_name(Some("muxus"));
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.add(&container);
    window.show_all();
    let titled = window.clone();
    let built = super::guarded(WebViewBuilder::new())
        .with_related_view(opener.clone())
        .with_focused(true)
        .with_document_title_changed_handler(move |title| {
            if !title.is_empty() {
                titled.set_title(&title);
            }
        })
        .build_gtk(&container);
    let page = match built {
        Ok(page) => page,
        Err(_) => {
            window.close();
            return None;
        }
    };
    let view = page.webview();
    attach(&view);
    // `window.close()` from the page closes its window.
    let closing = window.clone();
    view.connect_close(move |_| closing.close());
    let id = NEXT_PAGE_WINDOW.with(|next| {
        next.set(next.get() + 1);
        next.get()
    });
    PAGE_WINDOWS.with(|windows| windows.borrow_mut().insert(id, page));
    window.connect_destroy(move |_| {
        // Drop the view once GTK is done tearing the window down.
        glib::idle_add_local_once(move || {
            PAGE_WINDOWS.with(|windows| windows.borrow_mut().remove(&id));
        });
    });
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_why_a_certificate_is_not_trusted() {
        assert_eq!(
            describe(TlsCertificateFlags::UNKNOWN_CA | TlsCertificateFlags::EXPIRED),
            "It is not signed by an authority this computer trusts.\nIt has expired."
        );
        assert_eq!(
            describe(TlsCertificateFlags::GENERIC_ERROR),
            "It could not be checked."
        );
    }

    #[test]
    fn escapes_the_host_on_the_refusal_page() {
        assert!(refused_page("<x>").contains("&lt;x&gt;"));
    }

    #[test]
    fn the_menu_has_the_six_items_of_the_electron_window() {
        let shown = |back, forward, edit| -> Vec<Option<(&str, bool)>> {
            menu_entries(back, forward, edit)
                .iter()
                .map(|entry| entry.map(|(label, enabled, _)| (label, enabled)))
                .collect()
        };
        // A page with history behind it, right-clicked on its text.
        assert_eq!(
            shown(true, false, EditFlags::new(false, false)),
            [
                Some(("Back", true)),
                Some(("Forward", false)),
                Some(("Reload", true)),
                None,
                Some(("Cut", false)),
                Some(("Copy", false)),
                Some(("Paste", false)),
            ]
        );
        // Selected text in a field: everything to edit with.
        assert_eq!(
            EditFlags::new(true, true),
            EditFlags {
                can_cut: true,
                can_copy: true,
                can_paste: true
            }
        );
        // Selected page text copies; an empty field pastes.
        assert_eq!(
            EditFlags::new(false, true),
            EditFlags {
                can_cut: false,
                can_copy: true,
                can_paste: false
            }
        );
        assert_eq!(
            EditFlags::new(true, false),
            EditFlags {
                can_cut: false,
                can_copy: false,
                can_paste: true
            }
        );
    }
}
