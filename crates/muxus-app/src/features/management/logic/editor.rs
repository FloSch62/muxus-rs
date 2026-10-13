//! The request editor's words and rules (`RequestEditor.tsx`,
//! `GnmiForm.tsx`, `NetconfForm.tsx`): what the run button says, which
//! operations ask before they run, what the device must offer, and how the
//! number fields read what was typed.

use muxus_core::management::GnmiSubscriptionListMode;
use muxus_mgmt::workbench::requests::{
    Datastore, GnmiOperation, GnmiSetItemOp, ManagementDraft, NetconfDraft, NetconfOperation,
    netconf_changes_state,
};

/// `HOTKEY_MOD_LABEL`: `⌘` on macOS, `Ctrl+` elsewhere.
pub const HOTKEY_MOD_LABEL: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+"
};

/// What the run button says. `streaming`: the selected run is this draft's live subscription.
pub fn run_label(draft: &ManagementDraft, streaming: bool) -> &'static str {
    match draft {
        ManagementDraft::Gnmi(gnmi) => match gnmi.operation {
            GnmiOperation::Subscribe if streaming => "Stop",
            GnmiOperation::Subscribe => match gnmi.subscribe.list_mode {
                GnmiSubscriptionListMode::Stream => "Subscribe",
                GnmiSubscriptionListMode::Poll => "Start polling",
                GnmiSubscriptionListMode::Once => "Get once",
            },
            GnmiOperation::Set => "Review…",
            GnmiOperation::Get => "Get",
        },
        ManagementDraft::Netconf(netconf) => {
            if (netconf.operation == NetconfOperation::EditConfig
                && netconf.target == Datastore::Running)
                || (netconf_changes_state(netconf.operation)
                    && netconf.operation != NetconfOperation::EditConfig)
            {
                "Send…"
            } else {
                "Send"
            }
        }
    }
}

/// The icon in front of the run button's label.
pub fn run_icon(draft: &ManagementDraft, streaming: bool) -> &'static str {
    if streaming {
        "StopRounded"
    } else if matches!(draft, ManagementDraft::Gnmi(gnmi) if gnmi.operation == GnmiOperation::Set) {
        "RateReviewOutlined"
    } else {
        "PlayArrowRounded"
    }
}

/// The run button's tooltip.
pub fn run_tooltip(label: &str, problem: Option<&str>, connected: bool) -> String {
    match problem {
        Some(problem) => problem.to_owned(),
        None if connected => format!("{} · {HOTKEY_MOD_LABEL}Enter", label.replace('…', "")),
        None => "Not connected".into(),
    }
}

/// The capability a NETCONF operation needs.
pub fn netconf_requirement(operation: NetconfOperation) -> Option<&'static str> {
    match operation {
        NetconfOperation::Validate => Some(":validate"),
        NetconfOperation::Commit | NetconfOperation::DiscardChanges => Some(":candidate"),
        NetconfOperation::CancelCommit => Some(":confirmed-commit"),
        NetconfOperation::CreateSubscription => Some(":notification"),
        NetconfOperation::GetSchema => Some("ietf-netconf-monitoring"),
        _ => None,
    }
}

/// Why the operation menu warns about an operation, if the device lacks what it needs.
pub fn netconf_unsupported(
    operation: NetconfOperation,
    has_capability: impl Fn(&str) -> bool,
) -> Option<String> {
    let capability = netconf_requirement(operation)?;
    (!has_capability(capability)).then(|| format!("The device does not list {capability}"))
}

/// A confirmation asked before a NETCONF request runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub title: String,
    pub description: String,
    pub confirm_label: String,
    pub destructive: bool,
}

/// What to ask before sending `draft`, if anything.
pub fn netconf_confirmation(draft: &NetconfDraft) -> Option<Confirmation> {
    let operation = draft.operation;
    if netconf_changes_state(operation) && operation != NetconfOperation::EditConfig {
        let description = match operation {
            NetconfOperation::DiscardChanges => {
                "Every uncommitted change in the candidate is lost, including other sessions’."
                    .to_owned()
            }
            NetconfOperation::Commit if draft.confirmed => format!(
                "The candidate becomes the running configuration and rolls back after {} s unless you confirm.",
                super::format::js_number_string(draft.confirm_timeout)
            ),
            NetconfOperation::Commit => "The candidate becomes the running configuration.".into(),
            NetconfOperation::KillSession => {
                format!(
                    "NETCONF session {} is ended on the device.",
                    draft.session_id
                )
            }
            _ => format!("This changes the {} datastore.", draft.target.as_str()),
        };
        return Some(Confirmation {
            title: format!("Send {}?", operation.as_str()),
            description,
            confirm_label: operation.as_str().to_owned(),
            destructive: operation != NetconfOperation::Commit,
        });
    }
    if operation == NetconfOperation::EditConfig && draft.target == Datastore::Running {
        return Some(Confirmation {
            title: "Edit the running configuration?".into(),
            description: "The change takes effect on the device immediately, without a commit."
                .into(),
            confirm_label: "Apply".into(),
            destructive: true,
        });
    }
    None
}

/// The datastores a NETCONF form offers, given the device's capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datastores {
    /// Readable: running, plus candidate and startup when offered.
    pub readable: Vec<Datastore>,
    /// Edit targets: the writable ones, or running and candidate when none is listed.
    pub edit_targets: Vec<Datastore>,
}

pub fn datastores(has_capability: impl Fn(&str) -> bool) -> Datastores {
    let mut readable = vec![Datastore::Running];
    let mut writable = Vec::new();
    if has_capability(":writable-running") {
        writable.push(Datastore::Running);
    }
    if has_capability(":candidate") {
        readable.push(Datastore::Candidate);
        writable.push(Datastore::Candidate);
    }
    if has_capability(":startup") {
        readable.push(Datastore::Startup);
        writable.push(Datastore::Startup);
    }
    Datastores {
        readable,
        edit_targets: if writable.is_empty() {
            vec![Datastore::Running, Datastore::Candidate]
        } else {
            writable
        },
    }
}

/// The datastores the target toggle offers for an operation.
pub fn target_options(operation: NetconfOperation, stores: &Datastores) -> Vec<Datastore> {
    match operation {
        NetconfOperation::EditConfig => stores.edit_targets.clone(),
        NetconfOperation::DeleteConfig => stores
            .readable
            .iter()
            .copied()
            .filter(|store| *store != Datastore::Running)
            .collect(),
        _ => stores.readable.clone(),
    }
}

/// The caption under the NETCONF form for operations without fields.
pub fn netconf_caption(operation: NetconfOperation) -> Option<&'static str> {
    match operation {
        NetconfOperation::DiscardChanges => Some(
            "Throws away every uncommitted change in the candidate, yours and other sessions’.",
        ),
        NetconfOperation::Lock => Some(
            "Keeps other sessions from changing the datastore until you unlock it or this session ends.",
        ),
        NetconfOperation::Unlock => Some("Releases a lock this session holds."),
        NetconfOperation::Validate => {
            Some("Checks the datastore for errors without applying anything.")
        }
        _ => None,
    }
}

fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

/// `Number(text)` for what the number fields keep (digits and dots).
fn js_number(text: &str) -> f64 {
    if text.is_empty() {
        0.
    } else {
        text.parse().unwrap_or(f64::NAN)
    }
}

/// The Get form's depth: 1–64, or everything when empty.
pub fn parse_depth(text: &str) -> Option<u32> {
    let digits = digits(text);
    if digits.is_empty() {
        return None;
    }
    let value = js_number(&digits);
    Some(value.clamp(1., 64.) as u32)
}

/// The commit-confirmed timeout of a gNMI Set, at least one second.
pub fn parse_confirm_seconds(text: &str) -> u32 {
    let value = js_number(&digits(text));
    if value.is_nan() || value < 1. {
        1
    } else {
        value.min(f64::from(u32::MAX)) as u32
    }
}

/// `Number(text) || 1`: the NETCONF confirmed-commit timeout.
pub fn parse_confirm_timeout(text: &str) -> f64 {
    let value = js_number(&digits(text));
    if value.is_nan() || value == 0. {
        1.
    } else {
        value
    }
}

fn decimal(text: &str) -> f64 {
    js_number(
        &text
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>(),
    )
}

/// The sample interval: a positive number of seconds, 1 otherwise.
pub fn parse_sample_seconds(text: &str) -> f64 {
    let seconds = decimal(text);
    if seconds.is_finite() && seconds > 0. {
        seconds
    } else {
        1.
    }
}

/// The heartbeat interval: a positive number of seconds, or off.
pub fn parse_heartbeat(text: &str) -> Option<f64> {
    let seconds = decimal(text);
    (seconds > 0.).then_some(seconds)
}

/// Only digits (session ids).
pub fn digits_only(text: &str) -> String {
    digits(text)
}

/// Set operations: value, label, description.
pub const SET_OPS: &[(GnmiSetItemOp, &str, &str)] = &[
    (
        GnmiSetItemOp::Update,
        "Update",
        "Merge the value into the existing configuration",
    ),
    (
        GnmiSetItemOp::Replace,
        "Replace",
        "Replace everything at the path with the value",
    ),
    (
        GnmiSetItemOp::Delete,
        "Delete",
        "Remove the path and everything below it",
    ),
];

pub fn set_op_label(op: GnmiSetItemOp) -> &'static str {
    SET_OPS
        .iter()
        .find(|(value, _, _)| *value == op)
        .map_or("Update", |(_, label, _)| label)
}

/// The caption under a gNMI Set's rollback switch.
pub fn set_caption(confirm: bool) -> &'static str {
    if confirm {
        "A safety net for changes that could cut you off: the device undoes the change unless you confirm it in time (gNMI commit-confirmed extension)."
    } else {
        "Changes are shown as a diff against the device’s current configuration before anything is sent."
    }
}

/// The session's encoding a copied gnmic command uses when the request leaves it open.
pub fn copy_encoding(offered: &[muxus_core::management::GnmiEncoding]) -> Option<&'static str> {
    use muxus_core::management::GnmiEncoding;
    if offered.contains(&GnmiEncoding::JsonIetf) {
        Some("json_ietf")
    } else if offered.contains(&GnmiEncoding::Json) {
        Some("json")
    } else {
        None
    }
}

/// "Copy as …" choices: label, detail, icon.
pub fn copy_as_items(gnmi: bool) -> &'static [(&'static str, &'static str, &'static str)] {
    if gnmi {
        &[
            (
                "gnmic command",
                "Run the same request from a shell",
                "TerminalOutlined",
            ),
            (
                "Request JSON",
                "The request as Muxus sends it",
                "CodeOutlined",
            ),
        ]
    } else {
        &[
            (
                "RPC XML",
                "The <rpc> exactly as it goes on the wire",
                "CodeOutlined",
            ),
            (
                "ncclient script",
                "Python that sends this RPC",
                "CodeOutlined",
            ),
            (
                "ssh -s netconf command",
                "A raw NETCONF session in a terminal",
                "TerminalOutlined",
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_mgmt::workbench::requests::{default_gnmi_draft, default_netconf_draft};

    #[test]
    fn labels_the_run_button() {
        let mut gnmi = default_gnmi_draft();
        assert_eq!(
            run_label(&ManagementDraft::Gnmi(gnmi.clone()), false),
            "Get"
        );
        gnmi.operation = GnmiOperation::Set;
        let set = ManagementDraft::Gnmi(gnmi.clone());
        assert_eq!(run_label(&set, false), "Review…");
        assert_eq!(run_icon(&set, false), "RateReviewOutlined");
        gnmi.operation = GnmiOperation::Subscribe;
        assert_eq!(
            run_label(&ManagementDraft::Gnmi(gnmi.clone()), false),
            "Subscribe"
        );
        assert_eq!(
            run_label(&ManagementDraft::Gnmi(gnmi.clone()), true),
            "Stop"
        );
        gnmi.subscribe.list_mode = GnmiSubscriptionListMode::Poll;
        assert_eq!(
            run_label(&ManagementDraft::Gnmi(gnmi.clone()), false),
            "Start polling"
        );
        gnmi.subscribe.list_mode = GnmiSubscriptionListMode::Once;
        assert_eq!(run_label(&ManagementDraft::Gnmi(gnmi), false), "Get once");

        let mut netconf = default_netconf_draft();
        assert_eq!(
            run_label(&ManagementDraft::Netconf(netconf.clone()), false),
            "Send"
        );
        netconf.operation = NetconfOperation::EditConfig;
        assert_eq!(
            run_label(&ManagementDraft::Netconf(netconf.clone()), false),
            "Send"
        );
        netconf.target = Datastore::Running;
        assert_eq!(
            run_label(&ManagementDraft::Netconf(netconf.clone()), false),
            "Send…"
        );
        netconf.operation = NetconfOperation::Commit;
        assert_eq!(
            run_label(&ManagementDraft::Netconf(netconf), false),
            "Send…"
        );

        assert_eq!(
            run_tooltip("Review…", None, true),
            format!("Review · {HOTKEY_MOD_LABEL}Enter")
        );
        assert_eq!(
            run_tooltip("Get", Some("Enter a path."), true),
            "Enter a path."
        );
        assert_eq!(run_tooltip("Get", None, false), "Not connected");
    }

    #[test]
    fn asks_before_changing_state() {
        let mut draft = default_netconf_draft();
        assert_eq!(netconf_confirmation(&draft), None);
        draft.operation = NetconfOperation::Commit;
        let commit = netconf_confirmation(&draft).unwrap();
        assert_eq!(commit.title, "Send commit?");
        assert!(!commit.destructive);
        assert_eq!(
            commit.description,
            "The candidate becomes the running configuration."
        );
        draft.confirmed = true;
        draft.confirm_timeout = 120.;
        assert_eq!(
            netconf_confirmation(&draft).unwrap().description,
            "The candidate becomes the running configuration and rolls back after 120 s unless you confirm."
        );
        draft.operation = NetconfOperation::DeleteConfig;
        draft.target = Datastore::Startup;
        let delete = netconf_confirmation(&draft).unwrap();
        assert!(delete.destructive);
        assert_eq!(delete.description, "This changes the startup datastore.");
        assert_eq!(delete.confirm_label, "delete-config");
        draft.operation = NetconfOperation::EditConfig;
        assert_eq!(netconf_confirmation(&draft), None);
        draft.target = Datastore::Running;
        assert_eq!(netconf_confirmation(&draft).unwrap().confirm_label, "Apply");
    }

    #[test]
    fn offers_what_the_device_supports() {
        let none = datastores(|_| false);
        assert_eq!(none.readable, vec![Datastore::Running]);
        assert_eq!(
            none.edit_targets,
            vec![Datastore::Running, Datastore::Candidate]
        );
        let candidate = datastores(|fragment| fragment == ":candidate");
        assert_eq!(
            candidate.readable,
            vec![Datastore::Running, Datastore::Candidate]
        );
        assert_eq!(candidate.edit_targets, vec![Datastore::Candidate]);
        assert_eq!(
            target_options(NetconfOperation::DeleteConfig, &candidate),
            vec![Datastore::Candidate]
        );
        assert_eq!(
            netconf_unsupported(NetconfOperation::Commit, |_| false).as_deref(),
            Some("The device does not list :candidate")
        );
        assert_eq!(netconf_unsupported(NetconfOperation::Get, |_| false), None);
        assert_eq!(
            netconf_unsupported(NetconfOperation::GetSchema, |_| true),
            None
        );
    }

    #[test]
    fn reads_number_fields_like_the_form() {
        assert_eq!(parse_depth(""), None);
        assert_eq!(parse_depth("a"), None);
        assert_eq!(parse_depth("0"), Some(1));
        assert_eq!(parse_depth("3x"), Some(3));
        assert_eq!(parse_depth("999"), Some(64));
        assert_eq!(parse_confirm_seconds(""), 1);
        assert_eq!(parse_confirm_seconds("90s"), 90);
        assert_eq!(parse_confirm_timeout("0"), 1.);
        assert_eq!(parse_confirm_timeout("600"), 600.);
        assert_eq!(parse_sample_seconds("0.5"), 0.5);
        assert_eq!(parse_sample_seconds(""), 1.);
        assert_eq!(parse_sample_seconds("1.2.3"), 1.);
        assert_eq!(parse_heartbeat("30 s"), Some(30.));
        assert_eq!(parse_heartbeat(""), None);
        assert_eq!(digits_only("4a2"), "42");
        assert_eq!(set_op_label(GnmiSetItemOp::Replace), "Replace");
    }
}
