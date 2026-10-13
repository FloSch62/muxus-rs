//! What the result pane makes of a run (`ResultPane.tsx`): failures and
//! their hints, what an `<ok/>` means, the run to compare with, and the
//! quick starts of an empty workbench.

use std::sync::LazyLock;

use muxus_core::gnmi_path::{format_gnmi_path, try_parse_gnmi_path};
use muxus_core::js::js_trim;
use muxus_core::management::{
    GnmiDataType, GnmiSubscriptionListMode, GnmiSubscriptionMode, ManagementProtocol,
    ManagementResult,
};
use muxus_mgmt::workbench::requests::{
    Datastore, FilterType, GnmiDraft, GnmiOperation, GnmiSetItemOp, ManagementDraft, NetconfDraft,
    NetconfOperation, default_gnmi_draft, default_netconf_draft,
};
use muxus_mgmt::workbench::store::{RunRecord, RunStatus};
use regex::Regex;

static RPC_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(\w+:)?rpc-error[\s>]").expect("valid regex"));
static SAMPLE_INTERVAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)sample interval").expect("valid regex"));

/// A NETCONF reply that carried `<rpc-error>` counts as failed even though the RPC itself completed.
pub fn netconf_failed(run: &RunRecord) -> bool {
    matches!(&run.result, Some(ManagementResult::NetconfRpc { xml, .. }) if RPC_ERROR.is_match(xml))
}

/// The run failed, or its NETCONF reply did.
pub fn run_failed(run: &RunRecord) -> bool {
    run.status == RunStatus::Error || netconf_failed(run)
}

/// A hint under an error for the codes that have an obvious next step.
pub fn hint_for(code: Option<&str>, message: &str) -> Option<&'static str> {
    match code {
        Some("UNAUTHENTICATED") => {
            Some("The device rejected the login. Reconnect to enter the credentials again.")
        }
        Some("PERMISSION_DENIED") => {
            Some("The user may read but not change this, or not see this path at all.")
        }
        Some("UNIMPLEMENTED") => Some("The device does not implement this RPC or option."),
        Some("RESOURCE_EXHAUSTED") => {
            Some("Narrow the path, or ask for less with Depth or a data type.")
        }
        _ if SAMPLE_INTERVAL.is_match(message) => {
            Some("Raise the sample interval in the Subscribe options.")
        }
        _ => None,
    }
}

fn same_request(a: &ManagementDraft, b: &ManagementDraft) -> bool {
    match (a, b) {
        (ManagementDraft::Gnmi(a), ManagementDraft::Gnmi(b)) => {
            a.operation == b.operation && a.data_type == b.data_type && a.paths == b.paths
        }
        (ManagementDraft::Netconf(a), ManagementDraft::Netconf(b)) => {
            a.operation == b.operation && a.source == b.source && a.filter == b.filter
        }
        _ => false,
    }
}

fn result_op(
    result: Option<&ManagementResult>,
) -> Option<std::mem::Discriminant<ManagementResult>> {
    result.map(std::mem::discriminant)
}

/// The latest earlier successful run of the same request, to compare with.
pub fn previous_run<'a>(runs: &'a [RunRecord], run: &RunRecord) -> Option<&'a RunRecord> {
    let index = runs.iter().position(|candidate| candidate.id == run.id);
    let start = index.map_or(0, |index| index + 1);
    runs.get(start..)?.iter().find(|candidate| {
        candidate.status == RunStatus::Ok
            && result_op(candidate.result.as_ref()) == result_op(run.result.as_ref())
            && same_request(&candidate.draft, &run.draft)
    })
}

/// What an `<ok/>` means for the operation that got it, and what usually comes next.
pub fn ok_message(draft: &NetconfDraft) -> (String, Option<&'static str>) {
    let target = draft.target.as_str();
    match draft.operation {
        NetconfOperation::EditConfig if draft.target == Datastore::Candidate => (
            "The candidate took the change.".into(),
            Some("Nothing is live yet: review and commit it from the bar above."),
        ),
        NetconfOperation::EditConfig => (format!("The {target} configuration was changed."), None),
        NetconfOperation::Commit if draft.confirmed => (
            "Committed, pending confirmation.".into(),
            Some("Confirm before the timer runs out, or the device rolls back."),
        ),
        NetconfOperation::Commit => (
            "Committed.".into(),
            Some("The candidate is now the running configuration."),
        ),
        NetconfOperation::DiscardChanges => (
            "The candidate was reset to the running configuration.".into(),
            None,
        ),
        NetconfOperation::Validate => (
            format!("The {} configuration is valid.", draft.source.as_str()),
            None,
        ),
        NetconfOperation::Lock => (
            format!("This session now holds the {target} lock."),
            Some("Other sessions cannot change it until you unlock it or disconnect."),
        ),
        NetconfOperation::Unlock => (format!("The {target} lock was released."), None),
        NetconfOperation::CancelCommit => ("The confirmed commit was rolled back.".into(), None),
        NetconfOperation::CopyConfig => (
            format!("Copied {} to {target}.", draft.source.as_str()),
            None,
        ),
        NetconfOperation::DeleteConfig => (format!("The {target} datastore was deleted."), None),
        NetconfOperation::KillSession => (format!("Session {} was ended.", draft.session_id), None),
        NetconfOperation::CreateSubscription => (
            "Subscribed to notifications.".into(),
            Some("They appear as the device sends them."),
        ),
        _ => ("The device accepted the request.".into(), None),
    }
}

/// What the OK view says; an edit into a candidate that already matches running says so.
pub fn ok_view_message(
    draft: &NetconfDraft,
    candidate_clean: bool,
) -> (String, Option<&'static str>) {
    if draft.operation == NetconfOperation::EditConfig
        && draft.target == Datastore::Candidate
        && candidate_clean
    {
        return (
            "The candidate took the change.".into(),
            Some("It already matches the running configuration: there is nothing to commit."),
        );
    }
    ok_message(draft)
}

/// A suggested first request.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickStart {
    pub label: &'static str,
    pub detail: &'static str,
    /// `PlayArrowRounded` or `MonitorHeartOutlined`.
    pub icon: &'static str,
    pub draft: ManagementDraft,
}

fn get(paths: &[&str], depth: Option<u32>) -> ManagementDraft {
    ManagementDraft::Gnmi(GnmiDraft {
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        depth,
        ..default_gnmi_draft()
    })
}

fn watch(paths: &[&str], seconds: f64) -> ManagementDraft {
    let base = default_gnmi_draft();
    ManagementDraft::Gnmi(GnmiDraft {
        operation: GnmiOperation::Subscribe,
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        subscribe: muxus_mgmt::workbench::requests::GnmiSubscribeDraft {
            list_mode: GnmiSubscriptionListMode::Stream,
            mode: GnmiSubscriptionMode::Sample,
            sample_seconds: seconds,
            ..base.subscribe.clone()
        },
        ..base
    })
}

const PLAY: &str = "PlayArrowRounded";
const MONITOR: &str = "MonitorHeartOutlined";

/// The requests an empty workbench suggests, by what the device runs.
pub fn quick_starts(models: &[String], protocol: ManagementProtocol) -> Vec<QuickStart> {
    if protocol == ManagementProtocol::Netconf {
        return vec![
            QuickStart {
                label: "Running configuration",
                detail: "get-config from running",
                icon: PLAY,
                draft: ManagementDraft::Netconf(NetconfDraft {
                    operation: NetconfOperation::GetConfig,
                    source: Datastore::Running,
                    filter_type: FilterType::None,
                    ..default_netconf_draft()
                }),
            },
            QuickStart {
                label: "NETCONF sessions",
                detail: "Who else is connected",
                icon: PLAY,
                draft: ManagementDraft::Netconf(NetconfDraft {
                    operation: NetconfOperation::Get,
                    filter_type: FilterType::Subtree,
                    filter: "<netconf-state xmlns=\"urn:ietf:params:xml:ns:yang:ietf-netconf-monitoring\">\n  <sessions/>\n</netconf-state>".into(),
                    ..default_netconf_draft()
                }),
            },
        ];
    }
    let has = |fragment: &str| models.iter().any(|model| model.contains(fragment));
    let mut starts = Vec::new();
    if has("srl_nokia") {
        starts.extend([
            QuickStart {
                label: "System information",
                detail: "/system/information",
                icon: PLAY,
                draft: get(&["/system/information"], None),
            },
            QuickStart {
                label: "Interface counters, live",
                detail: "/interface[name=*]/statistics",
                icon: MONITOR,
                draft: watch(&["/interface[name=*]/statistics"], 2.),
            },
            QuickStart {
                label: "Interfaces at a glance",
                detail: "oper-state of every interface",
                icon: PLAY,
                draft: get(&["/interface[name=*]/oper-state"], None),
            },
            QuickStart {
                label: "Routing tables",
                detail: "/network-instance, two levels",
                icon: PLAY,
                draft: get(&["/network-instance"], Some(2)),
            },
        ]);
    } else if has("openconfig-interfaces") {
        starts.push(QuickStart {
            label: "Interface counters, live",
            detail: "/interfaces/interface/state/counters",
            icon: MONITOR,
            draft: watch(&["/interfaces/interface[name=*]/state/counters"], 5.),
        });
        starts.push(QuickStart {
            label: "Interface state",
            detail: "oper-status of every interface",
            icon: PLAY,
            draft: get(&["/interfaces/interface[name=*]/state/oper-status"], None),
        });
        if has("openconfig-system") {
            starts.push(QuickStart {
                label: "System state",
                detail: "/system/state",
                icon: PLAY,
                draft: get(&["/system/state"], None),
            });
        }
    }
    starts.push(QuickStart {
        label: "Top of the tree",
        detail: "/ with depth 2",
        icon: PLAY,
        draft: get(&["/"], Some(2)),
    });
    starts
}

/// The empty workbench's explanation.
pub fn empty_text(protocol: ManagementProtocol) -> &'static str {
    match protocol {
        ManagementProtocol::Gnmi => {
            "Browse the device in the explorer, or type a path above — it completes from the device’s own data. "
        }
        ManagementProtocol::Netconf => {
            "Browse the configuration in the explorer, or pick an operation above. "
        }
    }
}

/// The draft with `bad` replaced by `option` in every path (the error view's chips).
pub fn fix_path(draft: &GnmiDraft, bad: &str, option: &str) -> GnmiDraft {
    let replace = |path: &str| -> String {
        let Some(mut parsed) = try_parse_gnmi_path(path) else {
            return path.to_owned();
        };
        for elem in &mut parsed.elems {
            let local = match elem.name.rfind(':') {
                Some(colon) => &elem.name[colon + 1..],
                None => &elem.name,
            };
            if local == bad {
                elem.name = option.to_owned();
            }
        }
        format_gnmi_path(&parsed)
    };
    let mut next = draft.clone();
    next.paths = draft.paths.iter().map(|path| replace(path)).collect();
    for item in &mut next.set {
        item.path = replace(&item.path);
    }
    next
}

/// "Get the configuration now" after a Set: the paths it changed, as config.
pub fn verify_draft(draft: &GnmiDraft) -> Option<GnmiDraft> {
    let mut paths: Vec<String> = Vec::new();
    for item in &draft.set {
        let path = js_trim(&item.path);
        if item.op != GnmiSetItemOp::Delete && !path.is_empty() && !paths.iter().any(|p| p == path)
        {
            paths.push(path.to_owned());
        }
    }
    if paths.is_empty() {
        return None;
    }
    Some(GnmiDraft {
        operation: GnmiOperation::Get,
        paths,
        data_type: GnmiDataType::Config,
        ..draft.clone()
    })
}

/// A Set result's timestamp (nanoseconds as text) in milliseconds; 0 when absent.
pub fn set_timestamp_ms(timestamp: &str) -> u64 {
    let text = if timestamp.is_empty() { "0" } else { timestamp };
    text.parse::<u128>()
        .map(|nanos| (nanos / 1_000_000) as u64)
        .unwrap_or(0)
}

/// The leaf table leaves out the part of the path that was asked for when there was only one.
pub fn leaf_table_skip(requested: &[String]) -> usize {
    let asked: Vec<&String> = requested
        .iter()
        .filter(|path| !js_trim(path).is_empty())
        .collect();
    if asked.len() != 1 {
        return 0;
    }
    let length = requested
        .first()
        .and_then(|path| try_parse_gnmi_path(path))
        .map_or(1, |path| path.elems.len());
    length.saturating_sub(1)
}

/// The Get toolbar's counts.
pub fn get_counts(leaves: usize, notifications: usize) -> String {
    format!(
        "{} · {}",
        super::format::plural(leaves, "value", "values"),
        super::format::plural(notifications, "notification", "notifications")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::management::ManagementError;

    fn record(
        id: &str,
        draft: ManagementDraft,
        status: RunStatus,
        result: Option<ManagementResult>,
    ) -> RunRecord {
        RunRecord {
            id: id.into(),
            draft,
            label: "Get".into(),
            summary: String::new(),
            started_at: 0,
            status,
            duration_ms: None,
            bytes: None,
            error: None::<ManagementError>,
            result,
            live: None,
            synced: false,
            hidden: false,
        }
    }

    fn netconf_result(xml: &str) -> Option<ManagementResult> {
        Some(ManagementResult::NetconfRpc {
            xml: xml.into(),
            message_id: "1".into(),
        })
    }

    #[test]
    fn rpc_errors_fail_a_reply() {
        let draft = ManagementDraft::Netconf(default_netconf_draft());
        let ok = record(
            "a",
            draft.clone(),
            RunStatus::Ok,
            netconf_result("<rpc-reply><ok/></rpc-reply>"),
        );
        assert!(!netconf_failed(&ok));
        let failed = record(
            "b",
            draft,
            RunStatus::Ok,
            netconf_result("<nc:rpc-reply><nc:rpc-error>x</nc:rpc-error></nc:rpc-reply>"),
        );
        assert!(netconf_failed(&failed));
        assert!(run_failed(&failed));
    }

    #[test]
    fn hints_at_the_next_step() {
        assert!(
            hint_for(Some("UNAUTHENTICATED"), "")
                .unwrap()
                .starts_with("The device rejected")
        );
        assert_eq!(
            hint_for(None, "Sample Interval too small"),
            Some("Raise the sample interval in the Subscribe options.")
        );
        assert_eq!(hint_for(Some("INTERNAL"), "boom"), None);
    }

    #[test]
    fn finds_the_run_to_compare_with() {
        let get = |paths: &[&str]| get(paths, None);
        let result = Some(ManagementResult::GnmiGet {
            notifications: Vec::new(),
        });
        let runs = vec![
            record("new", get(&["/a"]), RunStatus::Ok, result.clone()),
            record("other", get(&["/b"]), RunStatus::Ok, result.clone()),
            record("failed", get(&["/a"]), RunStatus::Error, None),
            record("old", get(&["/a"]), RunStatus::Ok, result.clone()),
        ];
        assert_eq!(
            previous_run(&runs, &runs[0]).map(|run| run.id.as_str()),
            Some("old")
        );
        assert!(previous_run(&runs, &runs[3]).is_none());
    }

    #[test]
    fn explains_an_ok() {
        let mut draft = default_netconf_draft();
        draft.operation = NetconfOperation::EditConfig;
        assert_eq!(ok_message(&draft).0, "The candidate took the change.");
        assert_eq!(
            ok_view_message(&draft, true).1,
            Some("It already matches the running configuration: there is nothing to commit.")
        );
        draft.target = Datastore::Running;
        assert_eq!(
            ok_message(&draft),
            ("The running configuration was changed.".into(), None)
        );
        draft.operation = NetconfOperation::Lock;
        assert_eq!(
            ok_message(&draft).0,
            "This session now holds the running lock."
        );
        draft.operation = NetconfOperation::KillSession;
        draft.session_id = "42".into();
        assert_eq!(ok_message(&draft).0, "Session 42 was ended.");
        draft.operation = NetconfOperation::Get;
        assert_eq!(ok_message(&draft).0, "The device accepted the request.");
    }

    #[test]
    fn suggests_requests_by_device() {
        let srl = quick_starts(&["srl_nokia-interfaces".into()], ManagementProtocol::Gnmi);
        assert_eq!(
            srl.iter().map(|start| start.label).collect::<Vec<_>>(),
            vec![
                "System information",
                "Interface counters, live",
                "Interfaces at a glance",
                "Routing tables",
                "Top of the tree"
            ]
        );
        let ManagementDraft::Gnmi(watch) = &srl[1].draft else {
            panic!()
        };
        assert_eq!(watch.operation, GnmiOperation::Subscribe);
        assert_eq!(watch.subscribe.sample_seconds, 2.);
        let openconfig = quick_starts(
            &["openconfig-interfaces".into(), "openconfig-system".into()],
            ManagementProtocol::Gnmi,
        );
        assert_eq!(openconfig.len(), 4);
        assert_eq!(quick_starts(&[], ManagementProtocol::Gnmi).len(), 1);
        assert_eq!(quick_starts(&[], ManagementProtocol::Netconf).len(), 2);
    }

    #[test]
    fn fixes_paths_and_verifies_sets() {
        let mut draft = default_gnmi_draft();
        draft.paths = vec![
            "/srl:interfaces[name=e1]/state".into(),
            "not [parsed".into(),
        ];
        draft.set[0].path = "/interfaces/x".into();
        let fixed = fix_path(&draft, "interfaces", "interface");
        assert_eq!(
            fixed.paths,
            vec!["/interface[name=e1]/state", "not [parsed"]
        );
        assert_eq!(fixed.set[0].path, "/interface/x");

        assert_eq!(verify_draft(&default_gnmi_draft()), None);
        let mut set = default_gnmi_draft();
        set.operation = GnmiOperation::Set;
        set.set[0].path = " /a ".into();
        let mut second = set.set[0].clone();
        second.path = "/a".into();
        set.set.push(second);
        let verify = verify_draft(&set).unwrap();
        assert_eq!(verify.paths, vec!["/a"]);
        assert_eq!(verify.data_type, GnmiDataType::Config);
        assert_eq!(verify.operation, GnmiOperation::Get);

        assert_eq!(set_timestamp_ms(""), 0);
        assert_eq!(set_timestamp_ms("1700000000123456789"), 1_700_000_000_123);
        assert_eq!(leaf_table_skip(&["/a/b/c".into()]), 2);
        assert_eq!(leaf_table_skip(&["/a".into(), "/b".into()]), 0);
        assert_eq!(leaf_table_skip(&["".into()]), 0);
        assert_eq!(get_counts(1, 2), "1 value · 2 notifications");
    }
}
