//! TLS profiles and certificates (`tools/CertzTool.tsx`): replace a
//! profile's certificate, key and trust bundle, and check what the device
//! presents before making it final.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use muxus_core::management::{
    GnmiTlsMode, ManagementError, ManagementResult, PresentedCertificateInfo, grpc_methods,
};
use muxus_core::protocol::ManagementProfile;
use muxus_mgmt::workbench::connection::{RequestMessage, WorkbenchTransport};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, PathPromptOptions, SharedString, Styled,
    Window, div, px,
};
use muxus_ui::icon::{MEDIUM, icon};
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::display::Alert;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::super::logic::editor::Confirmation;
use super::super::logic::tools::rotation_version;
use super::super::ui::{ConfirmAction, Dialog, PromptAction};
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use super::kit::{body_text, caption_text, error_alert, error_of, message_error};
use super::rotation::{Rotation, RotationPhase, RotationState, RotationText};
use super::{ToolAction, contained, section, tool_header};
use muxus_ui::widgets::button::{Button, ButtonColor};
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::progress::CircularProgress;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PemFile {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CertzState {
    pub profiles: Option<Vec<String>>,
    pub loading: bool,
    pub profile: String,
    pub certificate: Option<PemFile>,
    pub key: Option<PemFile>,
    pub ca: Option<PemFile>,
    pub uploaded_fingerprint: Option<String>,
    pub presented: Option<PresentedCertificateInfo>,
    pub checking: bool,
    pub error: Option<ManagementError>,
    pub rotation: RotationState,
}

/// The PEM blocks in `text` whose label contains `kind`
/// (`/-----BEGIN ([A-Z0-9 ]+)-----[\s\S]*?-----END \1-----/g`).
pub fn pem_blocks(text: &str, kind: Option<&str>) -> Vec<String> {
    const BEGIN: &str = "-----BEGIN ";
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(found) = text[cursor..].find(BEGIN) {
        let start = cursor + found;
        let label_start = start + BEGIN.len();
        let label_len = text[label_start..]
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' '))
            .unwrap_or(text.len() - label_start);
        let label = &text[label_start..label_start + label_len];
        let after_label = label_start + label_len;
        if label.is_empty() || !text[after_label..].starts_with("-----") {
            cursor = start + 1;
            continue;
        }
        let end_marker = format!("-----END {label}-----");
        match text[after_label + 5..].find(&end_marker) {
            Some(offset) => {
                let end = after_label + 5 + offset + end_marker.len();
                if kind.is_none_or(|kind| label.contains(kind)) {
                    blocks.push(text[start..end].to_owned());
                }
                cursor = end;
            }
            None => cursor = start + 1,
        }
    }
    blocks
}

/// SHA-256 of a PEM certificate's DER, in the colon form TLS libraries print.
pub fn certificate_fingerprint(pem: &str) -> Option<String> {
    let mut body = String::new();
    let mut rest = pem;
    // `pem.replace(/-----[^-]+-----/g, '')`
    while let Some(start) = rest.find("-----") {
        let after = &rest[start + 5..];
        let label_end = after.find('-').unwrap_or(after.len());
        if label_end > 0 && after[label_end..].starts_with("-----") {
            body.push_str(&rest[..start]);
            rest = &after[label_end + 5..];
        } else {
            body.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
        }
    }
    body.push_str(rest);
    let body: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = BASE64.decode(body).ok()?;
    let digest = Sha256::digest(&der);
    Some(
        digest
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// A chain of certificates, leaf (or first) outermost, the way certz nests them through `parent`.
pub fn chain(certificates: &[String], key: Option<&str>) -> Option<Value> {
    let (first, rest) = certificates.split_first()?;
    let mut certificate = Map::new();
    certificate.insert("type".into(), json!("CERTIFICATE_TYPE_X509"));
    certificate.insert("encoding".into(), json!("CERTIFICATE_ENCODING_PEM"));
    certificate.insert(
        "raw_certificate".into(),
        json!(BASE64.encode(first.as_bytes())),
    );
    if let Some(key) = key {
        certificate.insert(
            "raw_private_key".into(),
            json!(BASE64.encode(key.as_bytes())),
        );
    }
    let mut link = Map::new();
    link.insert("certificate".into(), Value::Object(certificate));
    if let Some(parent) = chain(rest, None) {
        link.insert("parent".into(), parent);
    }
    Some(Value::Object(link))
}

impl CertzState {
    fn certificates(&self) -> Vec<String> {
        self.certificate
            .as_ref()
            .map(|file| pem_blocks(&file.text, Some("CERTIFICATE")))
            .unwrap_or_default()
    }

    fn key_block(&self) -> Option<String> {
        self.key.as_ref().and_then(|file| {
            pem_blocks(&file.text, Some("PRIVATE KEY"))
                .into_iter()
                .next()
        })
    }

    fn ca_blocks(&self) -> Vec<String> {
        self.ca
            .as_ref()
            .map(|file| pem_blocks(&file.text, Some("CERTIFICATE")))
            .unwrap_or_default()
    }

    /// What keeps the upload from starting.
    pub fn problem(&self) -> Option<&'static str> {
        if self.profile.is_empty() {
            Some("Choose a profile.")
        } else if self.certificates().is_empty() {
            Some("Choose a certificate (PEM).")
        } else if self.key_block().is_none() {
            Some("Choose its private key (PEM).")
        } else {
            None
        }
    }
}

/// Which PEM file a picker sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PemSlot {
    Certificate,
    Key,
    Ca,
}

impl ManagementView {
    pub(super) fn render_certz(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let can_run = self.grpc().is_some();
        let state: CertzState = self.tool("certz");
        let rotating = state.rotation.rotating();
        let reload = Button::new("certz-reload", "Reload")
            .disabled(!can_run || rotating)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| this.load_certz(cx)),
            )
            .into_any_element();
        let mut column = div().child(tool_header(
            "WorkspacePremiumOutlined",
            "Certificates",
            "gnsi.certz.v1.Certz",
            "Replace the certificate, key and trusted authorities of the device's TLS profiles. Muxus checks that the device presents the new certificate before you make it final.",
            Some(reload),
            &theme,
        ));
        if let Some(bar) = self.rotation_bar(
            "certz",
            RotationText {
                what: "certificate",
                plural: false,
                test_hint: "Check below that the device presents it.",
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
                        this.update_tool::<CertzState>("certz", |state| state.error = None);
                        cx.notify();
                    },
                ))),
            ));
        }

        // Profiles
        let add = Button::new("certz-add", "Add\u{2026}")
            .disabled(!can_run)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                    this.ui.prompt_initial = Some(String::new());
                    this.ui.dialog = Some(Dialog::Prompt {
                        title: "Add an SSL profile".into(),
                        description: None,
                        label: "Profile id".into(),
                        confirm_label: "Save".into(),
                        action: PromptAction::AddCertzProfile,
                        error: None,
                    });
                    cx.notify();
                }),
            )
            .into_any_element();
        let profiles: AnyElement = if state.loading {
            CircularProgress::new(("certz-progress", 1usize), 20., p.primary.main)
                .into_any_element()
        } else {
            let mut chips = div().flex().flex_wrap().gap(px(8.));
            for id in state.profiles.clone().unwrap_or_default() {
                let selected = state.profile == id;
                let choose = id.clone();
                let delete = id.clone();
                let mut chip = Chip::new(id.clone()).mono().on_click(
                    ("certz-chip", 1usize),
                    cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                        let profile = choose.clone();
                        this.update_tool::<CertzState>("certz", |state| state.profile = profile);
                        cx.notify();
                    }),
                );
                chip = if selected {
                    chip.palette(p.primary)
                } else {
                    chip.outlined()
                };
                if !rotating {
                    chip = chip.on_delete(
                        SharedString::from(format!("delete-profile-{id}")),
                        "DeleteOutlined",
                        Some("Delete profile".into()),
                        cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                            this.ask_delete_profile(&delete, cx)
                        }),
                    );
                }
                chips = chips.child(chip);
            }
            if state.profiles.as_ref().is_some_and(Vec::is_empty) {
                chips = chips.child(body_text(
                    "The device lists no profiles.",
                    p.text.secondary,
                    &theme,
                ));
            }
            chips.into_any_element()
        };
        column = column.child(section("SSL profiles", Some(add), profiles, &theme));

        // New certificate
        let certificates = state.certificates();
        let problem = state.problem();
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.pem_picker(
                "Certificate",
                state.certificate.as_ref(),
                PemSlot::Certificate,
                cx,
            ))
            .child(self.pem_picker("Private key", state.key.as_ref(), PemSlot::Key, cx))
            .child(self.pem_picker("CA bundle", state.ca.as_ref(), PemSlot::Ca, cx));
        if certificates.len() > 1 {
            form = form.child(caption_text(
                format!(
                    "{} certificates: the first is the device's, the rest its chain.",
                    certificates.len()
                ),
                p.text.secondary,
                &theme,
            ));
        }
        form = form.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pt(px(4.))
                .child(
                    contained("certz-upload", "Upload\u{2026}", ButtonColor::Primary)
                        .disabled(!can_run || problem.is_some() || rotating)
                        .on_click(cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| this.upload_certz(cx))),
                )
                .child(caption_text(
                    problem.unwrap_or(
                        "The key travels to the device inside this session\u{2019}s TLS connection; Muxus does not keep it.",
                    ),
                    p.text.secondary,
                    &theme,
                )),
        );
        let title = if state.profile.is_empty() {
            "New certificate".to_owned()
        } else {
            format!("New certificate for {}", state.profile)
        };
        column = column.child(section(&title, None, form, &theme));

        // What the device presents
        let plaintext = matches!(self.dialed_profile(), ManagementProfile::Gnmi(profile) if profile.tls == Some(GnmiTlsMode::Plaintext));
        if state.rotation.phase == RotationPhase::Pending || state.presented.is_some() {
            let check = Button::new(
                "certz-check",
                if state.checking {
                    "Checking \u{2026}"
                } else {
                    "Check now"
                },
            )
            .disabled(!can_run || state.checking || plaintext)
            .on_click(
                cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| this.check_certz(cx)),
            )
            .into_any_element();
            let body: AnyElement = if plaintext {
                Alert::new(
                    Status::Info,
                    "This session runs without TLS, so Muxus cannot see the certificate. Check it another way before finalizing.",
                )
                .into_any_element()
            } else if let Some(presented) = &state.presented {
                let matches = state.uploaded_fingerprint.as_ref() == Some(&presented.fingerprint);
                let tone = if matches {
                    p.success.main
                } else {
                    p.warning.main
                };
                let body2 = theme.typography.body2;
                div()
                    .border_1()
                    .rounded(px(theme.radius * 1.5))
                    .p(px(12.))
                    .border_color(tone)
                    .bg(alpha(tone, 0.06))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .mb(px(8.))
                            .child(if matches {
                                icon("CheckCircleRounded", MEDIUM, p.success.main)
                            } else {
                                icon("ErrorRounded", MEDIUM, p.warning.main)
                            })
                            .child(
                                div()
                                    .text_size(px(body2.size))
                                    .line_height(px(body2.line_height))
                                    .font_weight(muxus_ui::gpui_kit::FontWeight(650.))
                                    .child(if matches {
                                        "The device presents the new certificate."
                                    } else {
                                        "The device still presents a different certificate. This profile may not be the one its gRPC server uses."
                                    }),
                            ),
                    )
                    .child(caption_text(presented.subject.clone(), p.text.primary, &theme))
                    .child(caption_text(
                        format!("issued by {} \u{b7} valid until {}", presented.issuer, presented.valid_to),
                        p.text.secondary,
                        &theme,
                    ))
                    .child(
                        caption_text(presented.fingerprint.clone(), p.text.secondary, &theme)
                            .font_family(MONO)
                            .mt(px(4.)),
                    )
                    .into_any_element()
            } else {
                body_text(
                    "Muxus opens a new TLS connection and compares the certificate it gets with the one you uploaded.",
                    p.text.secondary,
                    &theme,
                )
                .into_any_element()
            };
            column = column.child(section(
                "What the device presents",
                Some(check),
                body,
                &theme,
            ));
        }
        let verify = self
            .controller
            .gnmi_info()
            .is_some_and(|info| info.tls.mode == GnmiTlsMode::Verify);
        if verify && state.rotation.phase == RotationPhase::Done {
            column = column.child(caption_text(
                "The next connection shows the new certificate for trust if it does not verify.",
                p.text.secondary,
                &theme,
            ));
        }
        column.into_any_element()
    }

    /// `PemPicker`: a label, Choose…, and the chosen file's name.
    fn pem_picker(
        &mut self,
        text: &'static str,
        file: Option<&PemFile>,
        slot: PemSlot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .w(px(120.))
                    .flex_none()
                    .text_size(px(body2.size))
                    .line_height(px(body2.line_height))
                    .child(text),
            )
            .child(
                Button::new(SharedString::from(format!("pick-{text}")), "Choose\u{2026}")
                    .outlined()
                    .start_icon("UploadFileOutlined")
                    .on_click(cx.listener_for_app(
                        move |this: &mut Self, cx: &mut Context<Self>| this.pick_pem(slot, cx),
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(12.5))
                    .line_height(px(body2.line_height))
                    .text_color(if file.is_some() {
                        p.text.primary
                    } else {
                        p.text.secondary
                    })
                    .when_some_font(file.is_some())
                    .child(file.map_or_else(|| "none".to_owned(), |file| file.name.clone())),
            )
            .into_any_element()
    }

    fn pick_pem(&mut self, slot: PemSlot, cx: &mut Context<Self>) {
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
            let text =
                std::fs::read(&path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                match text {
                    Ok(text) => this.update_tool::<CertzState>("certz", |state| {
                        let file = Some(PemFile { name, text });
                        match slot {
                            PemSlot::Certificate => state.certificate = file,
                            PemSlot::Key => state.key = file,
                            PemSlot::Ca => state.ca = file,
                        }
                    }),
                    Err(error) => {
                        this.toast(Status::Error, format!("Could not read {name}: {error}"), cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_certz(&mut self, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        self.update_tool::<CertzState>("certz", |state| {
            state.loading = true;
            state.error = None;
        });
        self.grpc_call(
            grpc_methods::CERTZ_PROFILES,
            json!({}),
            cx,
            |this, result, _| {
                this.update_tool::<CertzState>("certz", |state| {
                    state.loading = false;
                    match result {
                        Ok(response) => {
                            let profiles: Vec<String> = response
                                .get("ssl_profile_ids")
                                .and_then(Value::as_array)
                                .map(|ids| {
                                    ids.iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_owned)
                                        .collect()
                                })
                                .unwrap_or_default();
                            if state.profile.is_empty() {
                                state.profile = profiles.first().cloned().unwrap_or_default();
                            }
                            state.profiles = Some(profiles);
                        }
                        Err(error) => state.error = Some(error),
                    }
                })
            },
        );
        cx.notify();
    }

    /// The Add profile prompt was answered.
    pub(in super::super) fn add_certz_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let id = muxus_core::js::js_trim(&id).to_owned();
        if id.is_empty() {
            return;
        }
        self.grpc_call(
            grpc_methods::CERTZ_ADD_PROFILE,
            json!({ "ssl_profile_id": id }),
            cx,
            move |this, result, cx| match result {
                Ok(_) => {
                    this.update_tool::<CertzState>("certz", |state| state.profile = id);
                    this.load_certz(cx);
                }
                Err(error) => {
                    this.update_tool::<CertzState>("certz", |state| state.error = Some(error))
                }
            },
        );
    }

    fn ask_delete_profile(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.grpc().is_none() {
            return;
        }
        self.ui.dialog = Some(Dialog::Confirm {
            confirmation: Confirmation {
                title: format!("Delete the SSL profile {id}?"),
                description: "Services that use it lose their certificates.".into(),
                confirm_label: "Delete".into(),
                destructive: true,
            },
            action: ConfirmAction::Tool(ToolAction::DeleteCertzProfile(id.to_owned())),
        });
        cx.notify();
    }

    pub(super) fn delete_certz_profile(&mut self, id: String, cx: &mut Context<Self>) {
        self.grpc_call(
            grpc_methods::CERTZ_DELETE_PROFILE,
            json!({ "ssl_profile_id": id }),
            cx,
            move |this, result, cx| match result {
                Ok(_) => {
                    this.update_tool::<CertzState>("certz", |state| {
                        if state.profile == id {
                            state.profile.clear();
                        }
                    });
                    this.load_certz(cx);
                }
                Err(error) => {
                    this.update_tool::<CertzState>("certz", |state| state.error = Some(error))
                }
            },
        );
    }

    fn upload_certz(&mut self, cx: &mut Context<Self>) {
        let state: CertzState = self.tool("certz");
        if self.grpc().is_none() || state.problem().is_some() {
            return;
        }
        let certificates = state.certificates();
        let key = state.key_block();
        let ca = state.ca_blocks();
        let version = rotation_version(chrono::Utc::now());
        let created_on = chrono::Utc::now().timestamp();
        let mut entities = vec![json!({
            "version": version,
            "created_on": created_on,
            "certificate_chain": chain(&certificates, key.as_deref()),
        })];
        if !ca.is_empty() {
            entities.push(json!({ "version": version, "created_on": created_on, "trust_bundle": chain(&ca, None) }));
        }
        let fingerprint = certificate_fingerprint(&certificates[0]);
        self.update_tool::<CertzState>("certz", |state| {
            state.uploaded_fingerprint = fingerprint;
            state.presented = None;
        });
        let upload =
            json!({ "ssl_profile_id": state.profile, "certificates": { "entities": entities } });
        self.start_rotation(
            "certz",
            Rotation {
                method: grpc_methods::CERTZ_ROTATE,
                upload,
                finalize_field: "finalize_rotation",
                version,
            },
            cx,
        );
    }

    /// Connect again and see which certificate the device presents now.
    fn check_certz(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.grpc() else {
            return;
        };
        self.update_tool::<CertzState>("certz", |state| state.checking = true);
        let probe = self
            .on_runtime(async move { connection.request(RequestMessage::TlsProbe).outcome.await });
        cx.spawn(async move |this, cx| {
            let outcome = probe.await;
            let _ = this.update(cx, |this, cx| {
                this.update_tool::<CertzState>("certz", |state| match outcome {
                    Some(Ok(outcome)) => {
                        if let ManagementResult::TlsProbe { certificate } = outcome.result {
                            state.presented = Some(certificate);
                            state.checking = false;
                        }
                    }
                    Some(Err(error)) => {
                        state.checking = false;
                        state.error = Some(error_of(error));
                    }
                    None => {
                        state.checking = false;
                        state.error = Some(message_error("The session closed."));
                    }
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// Mono for a chosen file's name.
trait MonoWhen: Sized {
    fn when_some_font(self, mono: bool) -> Self;
}

impl MonoWhen for muxus_ui::gpui_kit::Div {
    fn when_some_font(self, mono: bool) -> Self {
        if mono { self.font_family(MONO) } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &str = "-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n";

    #[test]
    fn finds_pem_blocks_by_kind() {
        let bundle = format!(
            "junk\n{CERT}-----BEGIN PRIVATE KEY-----\nAQID\n-----END PRIVATE KEY-----\n-----BEGIN CERTIFICATE-----\nno end"
        );
        assert_eq!(
            pem_blocks(&bundle, Some("CERTIFICATE")),
            vec![CERT.trim_end().to_owned()]
        );
        assert_eq!(pem_blocks(&bundle, Some("PRIVATE KEY")).len(), 1);
        assert_eq!(pem_blocks(&bundle, None).len(), 2);
    }

    #[test]
    fn fingerprints_the_der() {
        let fingerprint = certificate_fingerprint(CERT).unwrap();
        // SHA-256 of the bytes 00 01 02.
        assert!(fingerprint.starts_with("AE:4B:32:80"));
        assert_eq!(fingerprint.len(), 32 * 3 - 1);
        assert_eq!(
            certificate_fingerprint("-----BEGIN X-----\n%%%\n-----END X-----"),
            None
        );
    }

    #[test]
    fn nests_chains_through_parent() {
        let chained = chain(&["a".into(), "b".into()], Some("k")).unwrap();
        assert_eq!(chained["certificate"]["raw_certificate"], "YQ==");
        assert_eq!(chained["certificate"]["raw_private_key"], "aw==");
        assert_eq!(chained["parent"]["certificate"]["raw_certificate"], "Yg==");
        assert!(
            chained["parent"]["certificate"]
                .get("raw_private_key")
                .is_none()
        );
        assert_eq!(chain(&[], None), None);
    }
}
