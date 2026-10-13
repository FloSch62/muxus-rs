//! Port of `tests/unit/client/command-buttons.test.ts`.

use muxus_store::prefs::command_buttons::*;
use serde_json::json;

fn button(id: &str, label: &str, command: &str, send_enter: bool) -> CommandButton {
    CommandButton {
        id: id.into(),
        label: label.into(),
        command: command.into(),
        send_enter,
        group_id: None,
        color: None,
        secret_id: None,
    }
}

fn filed_in(mut button: CommandButton, group: &str) -> CommandButton {
    button.group_id = Some(group.into());
    button
}

fn buttons() -> Vec<CommandButton> {
    vec![
        button("status", "Service status", "systemctl status nginx", true),
        button("disk", "Disk usage", "df -h", true),
        button(
            "restart",
            "Restart edge",
            "sudo systemctl restart edge",
            false,
        ),
    ]
}

fn ids(list: &[CommandButton]) -> Vec<&str> {
    list.iter().map(|entry| entry.id.as_str()).collect()
}

fn no_secrets(_: &str) -> Option<String> {
    None
}

#[test]
fn filter_matches_labels_and_command_text_case_insensitively() {
    let all = buttons();
    assert_eq!(
        filter_command_buttons(&all, "DISK", no_secrets),
        vec![&all[1]]
    );
    assert_eq!(
        filter_command_buttons(&all, "systemctl", no_secrets),
        vec![&all[0], &all[2]]
    );
    assert_eq!(
        filter_command_buttons(&all, "systemctl edge", no_secrets),
        vec![&all[2]]
    );
    assert_eq!(filter_command_buttons(&all, "   ", no_secrets).len(), 3);
}

#[test]
fn filter_finds_secret_buttons_by_secret_name_never_by_stale_text() {
    let mut enable = button("enable", "Enable", "old text", true);
    enable.secret_id = Some("s1".into());
    let mut all = buttons();
    all.push(enable.clone());
    let names = |id: &str| (id == "s1").then(|| "Core enable".to_owned());
    assert_eq!(filter_command_buttons(&all, "core", names), vec![&enable]);
    assert!(filter_command_buttons(&all, "old text", names).is_empty());
}

#[test]
fn secret_buttons_are_told_apart_and_labelled_without_a_command() {
    let mut secret = button("enable", "  ", "", true);
    secret.secret_id = Some("s1".into());
    assert!(is_secret_command_button(&secret));
    assert!(!is_secret_command_button(&buttons()[0]));
    // An empty reference is a secret button whose secret is not picked yet.
    let mut unpicked = secret.clone();
    unpicked.secret_id = Some(String::new());
    assert!(!is_secret_command_button(&unpicked));
    assert_eq!(command_button_label(&secret), "Secret");
    secret.label = "Enable".into();
    assert_eq!(command_button_label(&secret), "Enable");
}

#[test]
fn secret_buttons_keep_only_the_reference_when_restored() {
    let secret = json!({ "id": "enable", "label": "Enable", "command": "", "sendEnter": false, "secretId": "s1" });
    let groups = [default_command_group()];
    assert_eq!(
        serde_json::to_value(normalize_command_buttons(&json!([secret]), &groups).unwrap())
            .unwrap(),
        json!([secret])
    );
    let mut numeric = secret.clone();
    numeric["secretId"] = json!(42);
    let mut long = secret.clone();
    long["id"] = json!("b");
    long["secretId"] = json!("x".repeat(201));
    assert_eq!(
        serde_json::to_value(normalize_command_buttons(&json!([numeric, long]), &groups).unwrap())
            .unwrap(),
        json!([
            { "id": "enable", "label": "Enable", "command": "", "sendEnter": false },
            { "id": "b", "label": "Enable", "command": "", "sendEnter": false },
        ])
    );
}

#[test]
fn command_input_handles_enter_and_multiline_text() {
    assert_eq!(
        command_button_input(&button("uptime", "Uptime", "uptime", true)),
        "uptime\r"
    );
    assert_eq!(
        command_button_input(&button("danger", "Review", "systemctl restart app", false)),
        "systemctl restart app"
    );
    assert_eq!(
        command_button_input(&button("multi", "Multi", "one\ntwo\n", true)),
        "one\rtwo\r"
    );
}

#[test]
fn activation_returns_focus_after_sending() {
    let events = std::cell::RefCell::new(Vec::new());
    let sent = activate_command_button(
        |input| {
            assert_eq!(input, "uptime\r");
            events.borrow_mut().push("send");
            true
        },
        || events.borrow_mut().push("focus"),
        &button("uptime", "Uptime", "uptime", true),
    );
    assert!(sent);
    assert_eq!(*events.borrow(), vec!["send", "focus"]);
}

fn juniper() -> CommandButtonGroup {
    CommandButtonGroup {
        id: "juniper".into(),
        name: "Juniper".into(),
    }
}

fn huawei() -> CommandButtonGroup {
    CommandButtonGroup {
        id: "huawei".into(),
        name: "Huawei".into(),
    }
}

fn groups() -> Vec<CommandButtonGroup> {
    vec![default_command_group(), juniper(), huawei()]
}

fn filed() -> Vec<CommandButton> {
    vec![
        button("uptime", "Uptime", "uptime", true),
        filed_in(button("j-bgp", "BGP", "show bgp summary", true), "juniper"),
        filed_in(
            button("h-cur", "Config", "display current-configuration", true),
            "huawei",
        ),
        filed_in(
            button("j-int", "Interfaces", "show interfaces terse", true),
            "juniper",
        ),
        button("disk", "Disk", "df -h", true),
    ]
}

#[test]
fn groups_keep_the_default_first_and_once() {
    let general = json!({ "id": "default", "name": "General" });
    assert_eq!(
        serde_json::to_value(
            normalize_command_button_groups(&json!([juniper(), general, huawei()])).unwrap()
        )
        .unwrap(),
        json!([general, juniper(), huawei()])
    );
    assert_eq!(
        normalize_command_button_groups(&json!([juniper()])).unwrap(),
        vec![default_command_group(), juniper()]
    );
    assert_eq!(
        normalize_command_button_groups(&json!([])).unwrap(),
        vec![default_command_group()]
    );
}

#[test]
fn groups_drop_malformed_and_duplicate_entries_and_name_blank_ones() {
    assert_eq!(
        normalize_command_button_groups(&json!([
            juniper(),
            { "id": "juniper", "name": "Again" },
            { "id": "", "name": "No id" },
            { "id": "nameless" },
            "Cisco",
            { "id": "blank", "name": "   " },
        ]))
        .unwrap(),
        vec![
            default_command_group(),
            juniper(),
            CommandButtonGroup {
                id: "blank".into(),
                name: "Untitled group".into()
            }
        ]
    );
    assert_eq!(
        normalize_command_button_groups(&json!({ "default": "Default" })),
        None
    );
}

#[test]
fn buttons_saved_before_groups_stay_unchanged() {
    let all = buttons();
    assert_eq!(
        normalize_command_buttons(
            &serde_json::to_value(&all).unwrap(),
            &[default_command_group()]
        )
        .unwrap(),
        all
    );
}

#[test]
fn buttons_lose_unknown_colors_and_deleted_groups() {
    let all = buttons();
    let input = json!([
        { "id": "status", "label": "Service status", "command": "systemctl status nginx", "sendEnter": true, "color": "red", "groupId": "juniper" },
        { "id": "disk", "label": "Disk usage", "command": "df -h", "sendEnter": true, "color": "chartreuse", "groupId": "deleted" },
        { "id": "restart", "label": "Restart edge", "command": "sudo systemctl restart edge", "sendEnter": false, "groupId": "default" },
    ]);
    let mut first = filed_in(all[0].clone(), "juniper");
    first.color = Some(CommandButtonColor::Red);
    assert_eq!(
        normalize_command_buttons(&input, &groups()).unwrap(),
        vec![first, all[1].clone(), all[2].clone()]
    );
}

#[test]
fn buttons_drop_malformed_and_duplicate_entries_rather_than_every_button() {
    let all = buttons();
    let first = serde_json::to_value(&all[0]).unwrap();
    let mut second = serde_json::to_value(&all[1]).unwrap();
    second.as_object_mut().unwrap().shift_remove("sendEnter");
    assert_eq!(
        normalize_command_buttons(
            &json!([first, first, { "id": "x", "label": 1, "command": "" }, null, second]),
            &groups()
        )
        .unwrap(),
        vec![all[0].clone(), all[1].clone()]
    );
    assert_eq!(normalize_command_buttons(&json!("uptime"), &groups()), None);
}

#[test]
fn unfiled_buttons_and_buttons_of_deleted_groups_belong_to_the_default_group() {
    let all = filed();
    let groups = groups();
    assert_eq!(
        command_button_group_of(&all[0], &groups),
        DEFAULT_COMMAND_GROUP_ID
    );
    assert_eq!(
        command_button_group_of(&filed_in(all[0].clone(), "gone"), &groups),
        DEFAULT_COMMAND_GROUP_ID
    );
    assert_eq!(
        ids(&command_buttons_in_group(&all, &groups, "juniper")),
        vec!["j-bgp", "j-int"]
    );
    assert_eq!(
        ids(&command_buttons_in_group(
            &all,
            &groups,
            DEFAULT_COMMAND_GROUP_ID
        )),
        vec!["uptime", "disk"]
    );
}

#[test]
fn reorders_a_button_within_its_own_group() {
    let all = filed();
    let groups = groups();
    assert_eq!(
        ids(&move_command_button(&all, &groups, "j-int", -1)),
        vec!["uptime", "j-int", "h-cur", "j-bgp", "disk"]
    );
    assert_eq!(
        ids(&move_command_button(&all, &groups, "disk", -1)),
        vec!["disk", "j-bgp", "h-cur", "j-int", "uptime"]
    );
    assert_eq!(
        ids(&move_command_button(&all, &groups, "j-int", 1)),
        ids(&all)
    );
}

#[test]
fn moves_a_button_after_the_last_button_of_its_new_group() {
    let groups = groups();
    let moved = move_command_button_to_group(&filed(), &groups, "uptime", "juniper");
    assert_eq!(
        ids(&moved),
        vec!["j-bgp", "h-cur", "j-int", "uptime", "disk"]
    );
    assert_eq!(moved[3].group_id.as_deref(), Some("juniper"));
    let back = move_command_button_to_group(&moved, &groups, "j-bgp", DEFAULT_COMMAND_GROUP_ID);
    assert_eq!(
        back.iter()
            .find(|button| button.id == "j-bgp")
            .unwrap()
            .group_id,
        None
    );
    assert_eq!(
        ids(&back),
        vec!["h-cur", "j-int", "uptime", "disk", "j-bgp"]
    );
}

#[test]
fn duplicates_a_button_right_below_the_original() {
    let all: Vec<CommandButton> = filed()
        .into_iter()
        .map(|mut button| {
            if button.id == "j-bgp" {
                button.color = Some(CommandButtonColor::Purple);
            }
            button
        })
        .collect();
    let copied = duplicate_command_button(&all, "j-bgp", "copy");
    assert_eq!(
        ids(&copied),
        vec!["uptime", "j-bgp", "copy", "h-cur", "j-int", "disk"]
    );
    assert_eq!(
        serde_json::to_value(&copied[2]).unwrap(),
        json!({ "id": "copy", "label": "BGP copy", "command": "show bgp summary", "sendEnter": true, "groupId": "juniper", "color": "purple" })
    );
}

#[test]
fn never_moves_a_group_above_the_default_group() {
    let groups = groups();
    let group_ids =
        |list: Vec<CommandButtonGroup>| list.into_iter().map(|group| group.id).collect::<Vec<_>>();
    assert_eq!(
        group_ids(move_command_button_group(&groups, "huawei", -1)),
        vec!["default", "huawei", "juniper"]
    );
    assert_eq!(
        group_ids(move_command_button_group(&groups, "juniper", -1)),
        group_ids(groups.clone())
    );
    assert_eq!(
        group_ids(move_command_button_group(&groups, "default", 1)),
        group_ids(groups.clone())
    );
}

#[test]
fn deletes_a_group_with_its_buttons_and_keeps_the_default() {
    let all = filed();
    let groups = groups();
    assert_eq!(
        without_command_button_group(&all, &groups, "juniper"),
        (
            vec![all[0].clone(), all[2].clone(), all[4].clone()],
            vec![default_command_group(), huawei()]
        )
    );
    assert_eq!(
        without_command_button_group(&all, &groups, DEFAULT_COMMAND_GROUP_ID),
        (all.clone(), groups.clone())
    );
}

#[test]
fn names_new_groups_without_repeating_a_name() {
    let mut list = groups();
    assert_eq!(unused_command_button_group_name(&list), "New group");
    list.push(CommandButtonGroup {
        id: "a".into(),
        name: "New group".into(),
    });
    list.push(CommandButtonGroup {
        id: "b".into(),
        name: "new group 2".into(),
    });
    assert_eq!(unused_command_button_group_name(&list), "New group 3");
}

#[test]
fn shown_group_prefers_session_then_host_then_selected() {
    let groups = groups();
    assert_eq!(
        shown_command_button_group(&groups, Some("huawei"), Some("juniper"), Some("default")),
        "huawei"
    );
    assert_eq!(
        shown_command_button_group(&groups, None, Some("juniper"), Some("huawei")),
        "juniper"
    );
    assert_eq!(
        shown_command_button_group(&groups, None, None, Some("huawei")),
        "huawei"
    );
    assert_eq!(
        shown_command_button_group(&groups, None, None, None),
        DEFAULT_COMMAND_GROUP_ID
    );
    assert_eq!(
        shown_command_button_group(&groups, None, Some("cisco"), Some("huawei")),
        "huawei"
    );
    assert_eq!(
        shown_command_button_group(&groups, None, None, Some("cisco")),
        DEFAULT_COMMAND_GROUP_ID
    );
}

#[test]
fn colors_use_a_darker_ink_on_light_bars() {
    assert_eq!(CommandButtonColor::Red.ink(false), "#b91c1c");
    assert_eq!(CommandButtonColor::Red.ink(true), "#f87171");
    assert!(is_command_button_color(&json!("gray")));
    assert!(!is_command_button_color(&json!("#ff0000")));
}

#[test]
fn preference_ids_carry_a_uuid() {
    let id = new_preference_id("highlight");
    assert!(id.starts_with("highlight-"));
    assert_eq!(id.len(), "highlight-".len() + 36);
    assert_ne!(new_preference_id("highlight"), id);
}
