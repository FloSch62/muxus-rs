//! gNSI's rotate pattern, shared by authz, pathz, certz and credentialz
//! (`tools/rotation.tsx`): the upload takes effect at once but stays
//! provisional while the stream is open. Test it, then finalize; closing the
//! stream or losing the connection makes the device roll back.

use muxus_core::management::ManagementError;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use muxus_ui::icon::{MEDIUM, icon};
use muxus_ui::theme::{PaletteColor, Theme};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::logic::tools::elapsed;
use super::super::view::ManagementView;
use super::super::workbench::ListenerForApp;
use super::contained;
use super::kit::error_alert;
use muxus_ui::widgets::button::Button;
use muxus_ui::widgets::button::ButtonColor;
use muxus_ui::widgets::progress::CircularProgress;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RotationPhase {
    #[default]
    Idle,
    Uploading,
    Pending,
    Finalizing,
    Done,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RotationState {
    pub phase: RotationPhase,
    pub version: Option<String>,
    pub started_at: Option<i64>,
    pub error: Option<ManagementError>,
}

impl RotationState {
    /// Uploading, waiting for its finalize, or finalizing.
    pub fn rotating(&self) -> bool {
        matches!(
            self.phase,
            RotationPhase::Pending | RotationPhase::Uploading | RotationPhase::Finalizing
        )
    }
}

/// What a rotation sends and how it finalizes.
pub struct Rotation {
    pub method: &'static str,
    /// The upload message (upload_request, certificates, credential, …).
    pub upload: Value,
    /// Name of the finalize field in this service's request.
    pub finalize_field: &'static str,
    pub version: String,
}

/// What a tool's rotation bar says.
pub struct RotationText {
    pub what: &'static str,
    /// `what` names several things: "are", not "is".
    pub plural: bool,
    pub test_hint: &'static str,
}

/// The headline of the bar for a phase.
pub fn headline(phase: RotationPhase, what: &str, plural: bool) -> String {
    let verb = if plural { "are" } else { "is" };
    match phase {
        RotationPhase::Uploading => format!("Uploading the {what} \u{2026}"),
        RotationPhase::Pending => format!("The new {what} {verb} active, but not final"),
        RotationPhase::Finalizing => "Finalizing \u{2026}".into(),
        RotationPhase::Done => format!("The new {what} {verb} final"),
        _ => format!("Rolled back: the previous {what} {verb} back in force"),
    }
}

/// The phase once the stream ended.
pub fn ended(phase: RotationPhase, error: Option<&ManagementError>) -> Option<RotationPhase> {
    if error.is_some_and(|error| error.cancelled != Some(true)) {
        return Some(RotationPhase::Failed);
    }
    match phase {
        RotationPhase::Finalizing => Some(RotationPhase::Done),
        RotationPhase::Pending | RotationPhase::Uploading => Some(RotationPhase::RolledBack),
        _ => None,
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl ManagementView {
    /// The rotation in a tool's state.
    pub(super) fn rotation(&self, key: &str) -> RotationState {
        self.store.read(|state| {
            state
                .tools
                .get(key)
                .and_then(|tool| tool.get("rotation"))
                .and_then(|rotation| serde_json::from_value(rotation.clone()).ok())
                .unwrap_or_default()
        })
    }

    /// Change the rotation in a tool's state (`setRotation`).
    pub(super) fn update_rotation(
        &mut self,
        key: &'static str,
        update: impl FnOnce(&mut RotationState),
    ) {
        let mut rotation = self.rotation(key);
        update(&mut rotation);
        let value = serde_json::to_value(&rotation).unwrap_or(Value::Null);
        self.store.update(|state| {
            state.set_tool_state(key, |current| {
                let mut tool = current
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| Value::Object(Default::default()));
                if let Some(map) = tool.as_object_mut() {
                    map.insert("rotation".into(), value);
                }
                tool
            })
        });
    }

    /// `startRotation`: open the stream and upload; the bar follows the phases.
    pub(super) fn start_rotation(
        &mut self,
        key: &'static str,
        rotation: Rotation,
        cx: &mut Context<Self>,
    ) {
        if self.grpc().is_none() {
            return;
        }
        self.cancel_stream(key);
        let version = rotation.version.clone();
        self.update_rotation(key, |state| {
            state.phase = RotationPhase::Uploading;
            state.version = Some(version);
            state.started_at = Some(now_ms());
            state.error = None;
        });
        self.start_stream(
            key,
            rotation.method,
            None,
            Some(rotation.finalize_field),
            cx,
            move |this, _, _| {
                if this.rotation(key).phase == RotationPhase::Uploading {
                    this.update_rotation(key, |state| state.phase = RotationPhase::Pending);
                }
            },
            move |this, error, cx| {
                let phase = this.rotation(key).phase;
                if let Some(next) = ended(phase, error.as_ref()) {
                    this.update_rotation(key, |state| {
                        state.phase = next;
                        if next == RotationPhase::Failed {
                            state.error = error;
                        }
                    });
                    this.rotation_settled(key, next, cx);
                }
            },
        );
        self.stream_send(key, rotation.upload);
        cx.notify();
    }

    /// `finalizeRotation`.
    pub(super) fn finalize_rotation(&mut self, key: &'static str, cx: &mut Context<Self>) {
        if !self
            .ui
            .tools
            .streams
            .get(key)
            .is_some_and(|active| active.finalize_field.is_some())
        {
            return;
        }
        self.update_rotation(key, |state| state.phase = RotationPhase::Finalizing);
        self.finalize_stream(key);
        cx.notify();
    }

    /// `RotationBar`: where a rotation stands, with Finalize and Roll back while it is provisional.
    pub(super) fn rotation_bar(
        &mut self,
        key: &'static str,
        text: RotationText,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let p = &theme.palette;
        let rotation = self.rotation(key);
        let dismiss = move |this: &mut Self, cx: &mut Context<Self>| {
            this.update_rotation(key, |state| {
                state.phase = RotationPhase::Idle;
                state.error = None;
            });
            cx.notify();
        };
        match rotation.phase {
            RotationPhase::Idle => return None,
            RotationPhase::Failed => {
                let error = rotation
                    .error
                    .clone()
                    .unwrap_or_else(|| super::kit::message_error(""));
                return Some(
                    error_alert(
                        &error,
                        theme,
                        Some(std::rc::Rc::new(cx.listener_for_app(dismiss))),
                    )
                    .into_any_element(),
                );
            }
            _ => {}
        }
        let tone: PaletteColor = match rotation.phase {
            RotationPhase::Done => p.success,
            RotationPhase::RolledBack => p.info,
            _ => p.warning,
        };
        let state_icon = match rotation.phase {
            RotationPhase::Done => {
                icon("CheckCircleRounded", MEDIUM, p.success.main).into_any_element()
            }
            RotationPhase::RolledBack => {
                icon("UndoRounded", MEDIUM, p.info.main).into_any_element()
            }
            RotationPhase::Uploading | RotationPhase::Finalizing => {
                CircularProgress::new(("rotation-progress", 1usize), 18., p.primary.main)
                    .into_any_element()
            }
            _ => icon("HourglassTopRounded", MEDIUM, p.warning.main).into_any_element(),
        };
        let body2 = theme.typography.body2;
        let caption = theme.typography.caption;
        let mut headline_row = div()
            .flex()
            .flex_wrap()
            .items_baseline()
            .text_size(px(body2.size))
            .line_height(px(body2.line_height))
            .font_weight(muxus_ui::gpui_kit::FontWeight(650.))
            .child(headline(rotation.phase, text.what, text.plural));
        if let Some(version) = &rotation.version {
            headline_row = headline_row.child(
                div()
                    .ml(px(8.))
                    .text_size(px(caption.size))
                    .font_weight(muxus_ui::gpui_kit::FontWeight(400.))
                    .text_color(p.text.secondary)
                    .child(format!("version {version}")),
            );
        }
        let mut words = div().flex_1().min_w_0().child(headline_row);
        if rotation.phase == RotationPhase::Pending {
            let pending = rotation
                .started_at
                .map(|started| {
                    format!(
                        " \u{b7} pending {}",
                        elapsed(((now_ms() - started).max(0) as f64 / 1000.).round() as u64)
                    )
                })
                .unwrap_or_default();
            words = words.child(
                div()
                    .text_size(px(caption.size))
                    .line_height(px(caption.line_height))
                    .text_color(p.text.secondary)
                    .child(format!(
                        "{} Closing this tab or losing the connection rolls it back{pending}.",
                        text.test_hint
                    )),
            );
        }
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(state_icon)
            .child(words);
        if rotation.rotating() {
            row = row
                .child(
                    Button::new(format!("rollback-{key}"), "Roll back")
                        .color(ButtonColor::Inherit)
                        .disabled(rotation.phase == RotationPhase::Finalizing)
                        .on_click(
                            cx.listener_for_app(move |this: &mut Self, _| this.cancel_stream(key)),
                        ),
                )
                .child(
                    contained(format!("finalize-{key}"), "Finalize", ButtonColor::Warning)
                        .disabled(rotation.phase != RotationPhase::Pending)
                        .on_click(cx.listener_for_app(
                            move |this: &mut Self, cx: &mut Context<Self>| {
                                this.finalize_rotation(key, cx)
                            },
                        )),
                );
        } else {
            row = row.child(
                Button::new(format!("dismiss-{key}"), "Dismiss")
                    .color(ButtonColor::Inherit)
                    .on_click(cx.listener_for_app(dismiss)),
            );
        }
        Some(
            div()
                .border_1()
                .border_color(tone.main)
                .rounded(px(theme.radius * 2.))
                .p(px(12.))
                .mb(px(16.))
                .bg(alpha(tone.main, 0.08))
                .child(row)
                .into_any_element(),
        )
    }

    /// A rotation finished: the policy tools read the policy in force again.
    fn rotation_settled(
        &mut self,
        key: &'static str,
        phase: RotationPhase,
        cx: &mut Context<Self>,
    ) {
        if !matches!(phase, RotationPhase::Done | RotationPhase::RolledBack) {
            return;
        }
        if self.ui.tools.mounted.map(|(tool, _, _)| tool) != Some(key) {
            return;
        }
        match key {
            "authz" => self.load_authz(cx),
            "pathz" => self.load_pathz(cx),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_each_phase() {
        assert_eq!(
            headline(RotationPhase::Pending, "policy", false),
            "The new policy is active, but not final"
        );
        assert_eq!(
            headline(RotationPhase::Done, "path rules", true),
            "The new path rules are final"
        );
        assert_eq!(
            headline(RotationPhase::RolledBack, "certificate", false),
            "Rolled back: the previous certificate is back in force"
        );
        assert_eq!(
            serde_json::to_value(RotationPhase::RolledBack).unwrap(),
            serde_json::json!("rolled-back")
        );
    }

    #[test]
    fn settles_when_the_stream_ends() {
        let cancelled = ManagementError {
            message: "cancelled".into(),
            code: None,
            cancelled: Some(true),
        };
        let failed = ManagementError {
            message: "denied".into(),
            code: Some("PERMISSION_DENIED".into()),
            cancelled: None,
        };
        assert_eq!(
            ended(RotationPhase::Pending, Some(&cancelled)),
            Some(RotationPhase::RolledBack)
        );
        assert_eq!(
            ended(RotationPhase::Finalizing, None),
            Some(RotationPhase::Done)
        );
        assert_eq!(
            ended(RotationPhase::Uploading, Some(&failed)),
            Some(RotationPhase::Failed)
        );
        assert_eq!(ended(RotationPhase::Idle, None), None);
    }
}
