//! Development aid: put the workbench into the states the reference
//! screenshots show (`MGMT_SCENE` of the example), once the session is up.

use std::time::Duration;

use muxus_mgmt::workbench::requests::{
    GnmiDraft, GnmiOperation, GnmiSetItem, GnmiSetItemOp, ManagementDraft, default_gnmi_draft,
    new_item_id,
};
use muxus_mgmt::workbench::store::{NavigatorTab, ResultView, WorkbenchArea};
use muxus_ui::gpui_kit::{Context, Window};

use super::logic::results::quick_starts;
use super::view::{ManagementView, SessionPhase};

impl ManagementView {
    /// Apply a named scene once connected: `workbench`, `get`, `table`, `raw`,
    /// `history`, `device`, `live`, `set`, `review`, `operations`, `security`,
    /// `error`, or `tool-<id>` for one gNOI/gNSI tool.
    pub fn apply_scene_when_ready(
        &mut self,
        scene: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scene = scene.into();
        cx.spawn_in(window, async move |this, cx| {
            // Wait for the session, then for the explorer's first look, as the
            // reference captures wait for "Nothing run yet".
            for _ in 0..600 {
                let ready = this
                    .update(cx, |this, _| this.phase == SessionPhase::Connected)
                    .unwrap_or(true);
                if ready {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = this.update_in(cx, |this, window, cx| this.apply_scene(&scene, window, cx));
        })
        .detach();
    }

    fn quick_start(&mut self, label: &str, cx: &mut Context<Self>) {
        let models: Vec<String> = self
            .controller
            .gnmi_info()
            .map(|info| info.models.iter().map(|model| model.name.clone()).collect())
            .unwrap_or_default();
        if let Some(start) = quick_starts(&models, self.protocol())
            .into_iter()
            .find(|start| start.label == label)
        {
            self.store.update(|state| state.draft = start.draft.clone());
            self.controller.run(Some(start.draft), false);
        }
        cx.notify();
    }

    fn apply_scene(&mut self, scene: &str, window: &mut Window, cx: &mut Context<Self>) {
        match scene {
            "get" => self.quick_start("System information", cx),
            "table" | "raw" | "history" | "device" => {
                self.quick_start("System information", cx);
                self.store.update(|state| match scene {
                    "table" => state.result_view = ResultView::Table,
                    "raw" => state.result_view = ResultView::Raw,
                    "history" => state.set_navigator(NavigatorTab::History),
                    _ => state.set_navigator(NavigatorTab::Device),
                });
            }
            "live" => self.quick_start("Interface counters, live", cx),
            "set" | "review" => {
                let draft = GnmiDraft {
                    operation: GnmiOperation::Set,
                    set: vec![GnmiSetItem {
                        id: new_item_id(),
                        op: GnmiSetItemOp::Update,
                        path: "/system/name/host-name".into(),
                        value: "\"leaf1-renamed\"".into(),
                        encoding: muxus_core::management::GnmiSetEncoding::JsonIetf,
                    }],
                    ..default_gnmi_draft()
                };
                self.store
                    .update(|state| state.draft = ManagementDraft::Gnmi(draft));
                if scene == "review" {
                    self.run_draft(window, cx);
                }
            }
            "operations" => self
                .store
                .update(|state| state.area = WorkbenchArea::Operations),
            tool if tool.starts_with("tool-") => {
                let id = &tool["tool-".len()..];
                let area = if super::tools::OPERATIONS.iter().any(|entry| entry.id == id) {
                    WorkbenchArea::Operations
                } else {
                    WorkbenchArea::Security
                };
                self.store.update(|state| {
                    state.area = area;
                    state.set_tool(area, id);
                });
            }
            "security" => self
                .store
                .update(|state| state.area = WorkbenchArea::Security),
            "error" => {
                let draft = ManagementDraft::Gnmi(GnmiDraft {
                    paths: vec!["/interfaces".into()],
                    ..default_gnmi_draft()
                });
                self.store.update(|state| state.draft = draft.clone());
                self.controller.run(Some(draft), false);
            }
            _ => {}
        }
        cx.notify();
    }
}
