//! SSH host keys and authorized keys (`tools/CredentialzTool.tsx`).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use muxus_core::management::{ManagementError, grpc_methods};
use muxus_mgmt::workbench::tool_format::parse_authorized_keys;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, PathPromptOptions, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::super::fields::EditorKey;
use super::super::logic::tools::rotation_version;
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use super::kit::{ToolField, body_text, caption_text, default_user, error_alert};
use super::rotation::{Rotation, RotationState, RotationText};
use super::{contained, section, tool_header};
use muxus_ui::widgets::button::{Button, ButtonColor};
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::progress::CircularProgress;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HostKey {
    pub text: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CredentialzState {
    pub host_keys: Option<Vec<HostKey>>,
    pub loading: bool,
    /// `None` until edited: the session's user.
    pub account: Option<String>,
    pub keys: String,
    pub error: Option<ManagementError>,
    pub rotation: RotationState,
}

/// A host key from GetPublicKeys: its text, and its type in words.
pub fn host_key(key: &Value) -> HostKey {
    let text = key
        .get("public_key")
        .and_then(Value::as_str)
        .filter(|data| !data.is_empty())
        .and_then(|data| BASE64.decode(data).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    HostKey {
        text,
        kind: key.get("key_type").and_then(Value::as_str).map(|kind| {
            kind.replacen("KEY_TYPE_", "", 1)
                .replace('_', " ")
                .to_lowercase()
        }),
        description: key
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// The RotateAccountCredentials upload for `account` with these key lines.
pub fn credential_upload(account: &str, keys: &str, version: &str, created_on: i64) -> Value {
    let parsed = parse_authorized_keys(keys);
    let authorized: Vec<Value> = parsed
        .keys
        .iter()
        .map(|key| {
            let mut entry = Map::new();
            // gNSI wants the key data's base64 text itself as the bytes.
            entry.insert(
                "authorized_key".into(),
                json!(BASE64.encode(key.blob.as_bytes())),
            );
            entry.insert("key_type".into(), json!(key.key_type));
            if !key.comment.is_empty() {
                entry.insert("description".into(), json!(key.comment));
            }
            Value::Object(entry)
        })
        .collect();
    json!({
        "credential": {
            "credentials": [{
                "account": muxus_core::js::js_trim(account),
                "version": version,
                "created_on": created_on,
                "authorized_keys": authorized,
            }]
        }
    })
}

/// What keeps the keys from being replaced, or what replacing them does.
pub fn keys_caption(account: &str, keys: &str) -> (String, bool) {
    let parsed = parse_authorized_keys(keys);
    let account = muxus_core::js::js_trim(account);
    if account.is_empty() {
        return ("Enter the account.".into(), false);
    }
    if let Some(problem) = parsed.problem {
        return (problem, true);
    }
    if parsed.keys.is_empty() {
        return ("Add at least one public key.".into(), false);
    }
    let count = parsed.keys.len();
    (
        format!(
            "{count} {} every key {account} may log in with now.",
            if count == 1 {
                "key replaces"
            } else {
                "keys replace"
            }
        ),
        false,
    )
}

fn has_problem(account: &str, keys: &str) -> bool {
    let parsed = parse_authorized_keys(keys);
    muxus_core::js::js_trim(account).is_empty()
        || parsed.problem.is_some()
        || parsed.keys.is_empty()
}

impl ManagementView {
    pub(super) fn render_credentialz(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: CredentialzState = self.tool("credentialz");
        let rotating = state.rotation.rotating();
        let account = state
            .account
            .clone()
            .unwrap_or_else(|| default_user(self.session_address().as_deref()));
        let reload = Button::new("credentialz-reload", "Reload")
            .disabled(!can_run || rotating)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                    this.load_credentialz(cx)
                }),
            )
            .into_any_element();
        let mut column = div().child(tool_header(
            "KeyOutlined",
            "SSH credentials",
            "gnsi.credentialz.v1.Credentialz",
            "The device's SSH host keys, and the public keys an account may log in with. New keys stay provisional until you have tried them.",
            Some(reload),
            &theme,
        ));
        if let Some(bar) = self.rotation_bar(
            "credentialz",
            RotationText {
                what: "authorized keys",
                plural: true,
                test_hint: "Open an SSH session with one of the new keys to check it works.",
            },
            &theme,
            cx,
        ) {
            column = column.child(bar);
        }
        if let Some(error) = &state.error {
            column = column.child(error_alert(
                error,
                &theme,
                Some(std::rc::Rc::new(cx.listener_for_app(
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.update_tool::<CredentialzState>("credentialz", |state| {
                            state.error = None
                        });
                        cx.notify();
                    },
                ))),
            ));
        }
        let host_keys: AnyElement = if state.loading {
            CircularProgress::new(("credentialz-progress", 1usize), 20., p.primary.main)
                .into_any_element()
        } else {
            match state.host_keys.as_ref().filter(|keys| !keys.is_empty()) {
                Some(keys) => {
                    let mut list = div().flex().flex_col().gap(px(8.));
                    for key in keys {
                        let mut row = div().flex().items_start().gap(px(8.));
                        if let Some(kind) = &key.kind {
                            row = row.child(Chip::new(kind.clone()).height(20.).font_size(11.));
                        }
                        list = list.child(
                            row.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(MONO)
                                    .text_size(px(11.5))
                                    .line_height(px(11.5 * 1.5))
                                    .child(key.text.clone()),
                            ),
                        );
                    }
                    list.into_any_element()
                }
                None => body_text(
                    "The device lists no host keys over gNSI.",
                    p.text.secondary,
                    &theme,
                )
                .into_any_element(),
            }
        };
        column = column.child(section("Host keys", None, host_keys, &theme));

        let add_file = Button::new("credentialz-pub", "Add a .pub file\u{2026}")
            .start_icon("UploadFileOutlined")
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                    this.pick_public_key(cx)
                }),
            )
            .into_any_element();
        let (caption, error) = keys_caption(&account, &state.keys);
        let problem = has_problem(&account, &state.keys);
        let keys_editor = self.text_area(
            EditorKey::Tool("credentialz.keys"),
            &state.keys,
            (4, 12),
            12.,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA\u{2026} you@laptop",
            window,
            cx,
        );
        let form = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().flex().child(self.tool_field(
                ToolField::new("credentialz.account", "Account", account).width(220.),
                window,
                cx,
            )))
            .child(
                FormField::new(
                    "credentialz-keys",
                    FieldBody::Element(keys_editor.into_any_element()),
                )
                .label("Public keys, one per line")
                .shrink(true)
                .multiline(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        contained("credentialz-replace", "Replace keys", ButtonColor::Primary)
                            .disabled(!can_run || problem || rotating)
                            .on_click(cx.listener_for_app(
                                |this: &mut Self, cx: &mut Context<Self>| {
                                    this.rotate_credentialz(cx)
                                },
                            )),
                    )
                    .child(caption_text(
                        caption,
                        if error {
                            p.error.main
                        } else {
                            p.text.secondary
                        },
                        &theme,
                    )),
            );
        column
            .child(section("Authorized keys", Some(add_file), form, &theme))
            .into_any_element()
    }

    pub(super) fn load_credentialz(&mut self, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        self.update_tool::<CredentialzState>("credentialz", |state| {
            state.loading = true;
            state.error = None;
        });
        self.grpc_call(
            grpc_methods::CREDENTIALZ_PUBLIC_KEYS,
            json!({}),
            cx,
            |this, result, _| {
                this.update_tool::<CredentialzState>("credentialz", |state| {
                    state.loading = false;
                    match result {
                        Ok(response) => {
                            state.host_keys = Some(
                                response
                                    .get("public_keys")
                                    .and_then(Value::as_array)
                                    .map(|keys| keys.iter().map(host_key).collect())
                                    .unwrap_or_default(),
                            );
                        }
                        Err(error) => state.error = Some(error),
                    }
                })
            },
        );
        cx.notify();
    }

    fn pick_public_key(&mut self, cx: &mut Context<Self>) {
        let paths = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(paths) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read =
                std::fs::read(&path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let _ = this.update(cx, |this, cx| {
                match read {
                    Ok(text) => this.update_tool::<CredentialzState>("credentialz", |state| {
                        let current = muxus_core::js::js_trim(&state.keys).to_owned();
                        let added = muxus_core::js::js_trim(&text);
                        state.keys = if current.is_empty() {
                            added.to_owned()
                        } else {
                            format!("{current}\n{added}")
                        };
                    }),
                    Err(error) => this.toast(
                        Status::Error,
                        format!("Could not read the file: {error}"),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rotate_credentialz(&mut self, cx: &mut Context<Self>) {
        let state: CredentialzState = self.tool("credentialz");
        let account = state
            .account
            .clone()
            .unwrap_or_else(|| default_user(self.session_address().as_deref()));
        if self.grpc().is_none() || has_problem(&account, &state.keys) {
            return;
        }
        let version = rotation_version(chrono::Utc::now());
        let upload = credential_upload(
            &account,
            &state.keys,
            &version,
            chrono::Utc::now().timestamp(),
        );
        self.start_rotation(
            "credentialz",
            Rotation {
                method: grpc_methods::CREDENTIALZ_ROTATE_ACCOUNT,
                upload,
                finalize_field: "finalize",
                version,
            },
            cx,
        );
    }

    pub(super) fn credentialz_field(&mut self, name: &str, value: String) {
        self.update_tool::<CredentialzState>("credentialz", |state| match name {
            "credentialz.account" => state.account = Some(value),
            "credentialz.keys" => state.keys = value,
            _ => {}
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl you@laptop";

    #[test]
    fn decodes_host_keys() {
        let key = host_key(
            &json!({"public_key": BASE64.encode("ssh-rsa AAAA"), "key_type": "KEY_TYPE_ECDSA_P_256"}),
        );
        assert_eq!(key.text, "ssh-rsa AAAA");
        assert_eq!(key.kind.as_deref(), Some("ecdsa p 256"));
    }

    #[test]
    fn builds_the_upload() {
        let upload = credential_upload(" admin ", KEY, "v1", 7);
        let credential = &upload["credential"]["credentials"][0];
        assert_eq!(credential["account"], "admin");
        assert_eq!(
            credential["authorized_keys"][0]["key_type"],
            "KEY_TYPE_ED25519"
        );
        assert_eq!(
            credential["authorized_keys"][0]["description"],
            "you@laptop"
        );
        assert_eq!(
            credential["authorized_keys"][0]["authorized_key"],
            BASE64.encode("AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl")
        );
    }

    #[test]
    fn explains_what_replacing_does() {
        assert_eq!(keys_caption("", KEY), ("Enter the account.".into(), false));
        assert_eq!(
            keys_caption("admin", ""),
            ("Add at least one public key.".into(), false)
        );
        assert_eq!(
            keys_caption("admin", KEY),
            (
                "1 key replaces every key admin may log in with now.".into(),
                false
            )
        );
        assert!(keys_caption("admin", "ssh-foo AAAA").1);
    }
}
