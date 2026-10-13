//! Hand a request over to automation: the same request as a gnmic command
//! line or an ncclient script. Passwords are never included; gnmic and
//! ncclient prompt or read them from the environment (port of
//! `client/src/management/copy-as.ts`).

use muxus_core::js::js_trim;
use muxus_core::management::{
    GnmiDataType, GnmiSubscriptionListMode, GnmiSubscriptionMode, GnmiTlsMode,
};
use muxus_core::protocol::{GnmiProfile, NetconfProfile};

use super::requests::{
    GnmiDraft, GnmiOperation, GnmiSetItemOp, NetconfDraft, netconf_rpc_body, non_empty_paths,
};
use super::xml::pretty_fragment;

/// Quote for POSIX shells.
pub fn shell_quote(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn gnmic_connection(profile: &GnmiProfile) -> Vec<String> {
    let mut args = vec![
        "gnmic".to_owned(),
        "-a".to_owned(),
        shell_quote(&format!("{}:{}", profile.host, profile.port)),
    ];
    if let Some(username) = profile.username.as_deref().filter(|name| !name.is_empty()) {
        args.extend([
            "-u".to_owned(),
            shell_quote(username),
            "-p".to_owned(),
            "\"$GNMIC_PASSWORD\"".to_owned(),
        ]);
    }
    match profile.tls.unwrap_or_default() {
        GnmiTlsMode::Plaintext => args.push("--insecure".into()),
        GnmiTlsMode::SkipVerify => args.push("--skip-verify".into()),
        GnmiTlsMode::Verify => {}
    }
    for (flag, value) in [
        ("--tls-ca", &profile.ca_file),
        ("--tls-cert", &profile.cert_file),
        ("--tls-key", &profile.key_file),
        ("--tls-server-name", &profile.tls_server_name),
    ] {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            args.push(flag.into());
            args.push(shell_quote(value));
        }
    }
    args
}

fn encoding_name(encoding: muxus_core::management::GnmiEncoding) -> String {
    serde_json::to_value(encoding)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The gnmic command for a host, optionally running a request.
pub fn gnmic_command(
    profile: &GnmiProfile,
    draft: Option<&GnmiDraft>,
    session_encoding: Option<&str>,
) -> String {
    let mut args = gnmic_connection(profile);
    let encoding = draft
        .and_then(|draft| draft.encoding)
        .or(profile.encoding)
        .map(encoding_name)
        .or_else(|| session_encoding.map(str::to_owned));
    if let Some(encoding) = encoding.filter(|encoding| !encoding.is_empty()) {
        args.push("-e".into());
        args.push(encoding);
    }
    let Some(draft) = draft else {
        args.push("capabilities".into());
        return args.join(" ");
    };
    let prefix = js_trim(&draft.prefix);
    if !prefix.is_empty() {
        args.push("--prefix".into());
        args.push(shell_quote(prefix));
    }
    match draft.operation {
        GnmiOperation::Get => {
            args.push("get".into());
            for path in non_empty_paths(&draft.paths) {
                args.push("--path".into());
                args.push(shell_quote(&path));
            }
            if draft.data_type != GnmiDataType::All {
                args.push("--type".into());
                args.push(
                    serde_json::to_value(draft.data_type)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                );
            }
            if let Some(depth) = draft.depth.filter(|depth| *depth > 0) {
                args.push("--depth".into());
                args.push(depth.to_string());
            }
        }
        GnmiOperation::Set => {
            args.push("set".into());
            for item in draft
                .set
                .iter()
                .filter(|item| !js_trim(&item.path).is_empty())
            {
                let path = shell_quote(js_trim(&item.path));
                let op = match item.op {
                    GnmiSetItemOp::Delete => {
                        args.push("--delete".into());
                        args.push(path);
                        continue;
                    }
                    GnmiSetItemOp::Update => "update",
                    GnmiSetItemOp::Replace => "replace",
                };
                args.push(format!("--{op}-path"));
                args.push(path);
                args.push(format!("--{op}-value"));
                args.push(shell_quote(&compact_json(&item.value)));
            }
        }
        GnmiOperation::Subscribe => {
            let sub = &draft.subscribe;
            args.push("subscribe".into());
            for path in non_empty_paths(&draft.paths) {
                args.push("--path".into());
                args.push(shell_quote(&path));
            }
            let list_mode = match sub.list_mode {
                GnmiSubscriptionListMode::Stream => "stream",
                GnmiSubscriptionListMode::Once => "once",
                GnmiSubscriptionListMode::Poll => "poll",
            };
            args.push("--mode".into());
            args.push(list_mode.into());
            if sub.list_mode == GnmiSubscriptionListMode::Stream {
                let mode = match sub.mode {
                    GnmiSubscriptionMode::TargetDefined => "target-defined",
                    GnmiSubscriptionMode::OnChange => "on-change",
                    GnmiSubscriptionMode::Sample => "sample",
                };
                args.push("--stream-mode".into());
                args.push(mode.into());
                if sub.mode == GnmiSubscriptionMode::Sample {
                    args.push("--sample-interval".into());
                    args.push(format!("{}s", js_seconds(sub.sample_seconds)));
                }
                if sub.suppress_redundant {
                    args.push("--suppress-redundant".into());
                }
                if let Some(seconds) = sub.heartbeat_seconds.filter(|seconds| *seconds > 0.0) {
                    args.push("--heartbeat-interval".into());
                    args.push(format!("{}s", js_seconds(seconds)));
                }
            }
            if sub.updates_only {
                args.push("--updates-only".into());
            }
        }
    }
    args.join(" ")
}

/// A number the way JavaScript prints it in a template string.
fn js_seconds(value: f64) -> String {
    match serde_json::Number::from_f64(value) {
        Some(number) => super::data_tree::js_number_text(&number),
        None => value.to_string(),
    }
}

fn compact_json(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|_| text.to_owned()),
        Err(_) => text.to_owned(),
    }
}

fn python_string(text: &str) -> String {
    if !text.contains("\"\"\"") && !text.ends_with('"') {
        return format!("\"\"\"{text}\"\"\"");
    }
    serde_json::to_string(text).unwrap_or_default()
}

/// A runnable ncclient script for a NETCONF request.
pub fn ncclient_script(profile: &NetconfProfile, draft: &NetconfDraft) -> String {
    let body = pretty_fragment(&netconf_rpc_body(draft));
    let mut lines = vec![
        "import getpass".to_owned(),
        String::new(),
        "from ncclient import manager".to_owned(),
        "from ncclient.xml_ import to_ele".to_owned(),
        String::new(),
        format!("RPC = {}", python_string(&format!("\n{body}\n"))),
        String::new(),
        "with manager.connect(".to_owned(),
        format!(
            "    host={},",
            serde_json::to_string(&profile.host).unwrap_or_default()
        ),
        format!("    port={},", profile.port),
    ];
    if let Some(username) = profile.username.as_deref().filter(|name| !name.is_empty()) {
        lines.push(format!(
            "    username={},",
            serde_json::to_string(username).unwrap_or_default()
        ));
    }
    lines.extend(
        [
            "    password=getpass.getpass(),",
            "    hostkey_verify=False,",
            ") as m:",
            "    reply = m.dispatch(to_ele(RPC))",
            "    print(reply.xml)",
            "",
        ]
        .map(str::to_owned),
    );
    lines.join("\n")
}

/// `ssh -s netconf` that a person can paste RPCs into by hand.
pub fn netconf_ssh_command(profile: &NetconfProfile) -> String {
    let user = profile
        .username
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| format!("{}@", shell_quote(name)))
        .unwrap_or_default();
    format!(
        "ssh -p {} {user}{} -s netconf",
        profile.port,
        shell_quote(&profile.host)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::requests::{NetconfOperation, default_gnmi_draft, default_netconf_draft};

    #[test]
    fn copies_requests_as_gnmic_and_ncclient_without_passwords() {
        let profile = GnmiProfile {
            profile_id: None,
            host: "leaf1".into(),
            port: 57400,
            username: Some("admin".into()),
            tls: Some(GnmiTlsMode::SkipVerify),
            tls_server_name: None,
            ca_file: None,
            cert_file: None,
            key_file: None,
            ssh_gateway: None,
            encoding: None,
        };
        let draft = GnmiDraft {
            paths: vec!["/interface[name=ethernet-1/1]".into()],
            data_type: GnmiDataType::State,
            ..default_gnmi_draft()
        };
        assert_eq!(
            gnmic_command(&profile, Some(&draft), Some("json_ietf")),
            "gnmic -a leaf1:57400 -u admin -p \"$GNMIC_PASSWORD\" --skip-verify -e json_ietf get --path '/interface[name=ethernet-1/1]' --type state"
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        let netconf = NetconfProfile {
            profile_id: None,
            host: "leaf1".into(),
            port: 830,
            username: Some("admin".into()),
            ssh_gateway: None,
        };
        let script = ncclient_script(
            &netconf,
            &NetconfDraft {
                operation: NetconfOperation::GetConfig,
                ..default_netconf_draft()
            },
        );
        assert!(script.contains("m.dispatch(to_ele(RPC))"));
        assert!(script.contains("password=getpass.getpass()"));
        assert!(
            script.contains("<get-config>\n  <source>\n    <running/>\n  </source>\n</get-config>"),
            "{script}"
        );
        assert_eq!(
            netconf_ssh_command(&netconf),
            "ssh -p 830 admin@leaf1 -s netconf"
        );
        let subscribe = GnmiDraft {
            operation: GnmiOperation::Subscribe,
            paths: vec!["/a".into()],
            ..default_gnmi_draft()
        };
        assert!(gnmic_command(&profile, Some(&subscribe), None).ends_with(
            "subscribe --path /a --mode stream --stream-mode sample --sample-interval 10s"
        ));
        assert!(gnmic_command(&profile, None, None).ends_with(" capabilities"));
    }
}
