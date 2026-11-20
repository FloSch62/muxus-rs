//! Port of tests/unit/server/password-managers-cli.test.ts. The stand-in programs are
//! shell scripts, so these run on Unix only.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use muxus_core::api::{PasswordManagerConfig, PasswordManagerEntry, PasswordManagerState};
use muxus_passwords::{
    BitwardenProvider, CliOptions, DashlaneProvider, HostEnv, LastPassProvider,
    OnePasswordProvider, PassProvider, PasswordManagerProblem, PasswordManagerProvider,
    PasswordManagerResult, ProtonPassProvider, ProviderStatus, run_cli,
};
use serde_json::json;

/// A program that behaves as `body` says, and the record of how it was called.
struct Fake {
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl Fake {
    /// `body` is shell. `has a b` is true when every word is among the arguments, `out`
    /// prints and ends the run, `fail` complains on stderr, and `$DIR` holds the files
    /// written with [`Fake::file`].
    fn new(name: &str, body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join(name);
        let script = format!(
            r#"#!/bin/sh
DIR='{dir}'
{{ for arg in "$@"; do printf '%s\037' "$arg"; done; printf '\n'; }} >> "$DIR/calls.log"
ALL='|'
for arg in "$@"; do ALL="$ALL$arg|"; done
has() {{
  for want in "$@"; do
    case "$ALL" in *"|$want|"*) ;; *) return 1 ;; esac
  done
  return 0
}}
out() {{ printf '%s' "$1"; exit 0; }}
fail() {{ printf '%s\n' "$1" >&2; exit "${{2:-1}}"; }}
{body}
fail "unexpected arguments: $*" 64
"#,
            dir = dir.path().display()
        );
        write_program(&program, &script);
        Self { dir, program }
    }

    /// A file the program can print with `cat "$DIR/<name>"`.
    fn file(self, name: &str, content: &str) -> Self {
        std::fs::write(self.dir.path().join(name), content).unwrap();
        self
    }

    fn program(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    fn config(&self) -> PasswordManagerConfig {
        PasswordManagerConfig {
            program: Some(self.program()),
            ..Default::default()
        }
    }

    /// The arguments of every run so far.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.dir.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let mut args: Vec<String> = line.split('\u{1f}').map(str::to_owned).collect();
                args.pop();
                args
            })
            .collect()
    }
}

/// Write a script another process is about to run. A shell writes it: a file this process
/// held open for writing could still be open in a child another test forked at that
/// moment, and running it would fail with "Text file busy".
fn write_program(program: &Path, script: &str) {
    use std::io::Write;
    let mut writer = std::process::Command::new("sh")
        .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
        .arg(program)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success());
}

fn problem_of<T>(result: PasswordManagerResult<T>) -> Option<PasswordManagerProblem> {
    result.err().map(|error| error.problem)
}

fn entry(
    id: &str,
    title: &str,
    username: Option<&str>,
    url: Option<&str>,
    group: Option<&str>,
) -> PasswordManagerEntry {
    PasswordManagerEntry {
        id: id.to_owned(),
        title: title.to_owned(),
        username: username.map(str::to_owned),
        url: url.map(str::to_owned),
        group: group.map(str::to_owned),
    }
}

fn host() -> HostEnv {
    HostEnv::system()
}

// --- running a password manager program ---------------------------------------------------

#[tokio::test]
async fn reports_a_missing_program_and_kills_one_that_never_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("not-installed");
    let error = run_cli(
        &host(),
        &missing.to_string_lossy(),
        &["x"],
        CliOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::Unavailable);
    assert_eq!(
        error.message,
        format!("{} was not found on this computer.", missing.display())
    );

    let hangs = Fake::new("hangs", "sleep 60");
    let started = std::time::Instant::now();
    let error = run_cli(
        &host(),
        &hangs.program(),
        &[] as &[&str],
        CliOptions::timeout(Duration::from_millis(300)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::Timeout);
    assert_eq!(
        error.message,
        "hangs did not answer in time. If it is waiting for you to approve or unlock, do that and try again."
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn hands_a_program_its_input_and_environment_and_collects_what_it_prints() {
    let echo = Fake::new(
        "echo",
        r#"read line
printf 'out:%s:%s:%s' "$line" "$MUXUS_TEST_ADDED" "${MUXUS_TEST_REMOVED-unset}"
printf 'warning\n' >&2
exit 3"#,
    );
    let host = host().with_var("MUXUS_TEST_REMOVED", "inherited");
    let result = run_cli(
        &host,
        &echo.program(),
        &["a b", "c"],
        CliOptions {
            input: Some("typed\n".into()),
            ..CliOptions::default()
        }
        .with_env("MUXUS_TEST_ADDED", Some("added"))
        .with_env("MUXUS_TEST_REMOVED", None),
    )
    .await
    .unwrap();
    assert_eq!(result.code, 3);
    assert_eq!(result.stdout, "out:typed:added:unset");
    assert_eq!(result.stderr, "warning\n");
    // Arguments reach the program as they are: nothing goes through a shell.
    assert_eq!(echo.calls(), [["a b", "c"]]);

    // A program that never reads its input is not waited on for it.
    let deaf = Fake::new("deaf", "out done");
    let result = run_cli(
        &host,
        &deaf.program(),
        &[] as &[&str],
        CliOptions {
            input: Some("x".repeat(1024 * 1024)),
            ..CliOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!((result.code, result.stdout.as_str()), (0, "done"));
}

// --- 1Password -----------------------------------------------------------------------------

fn op() -> Fake {
    Fake::new(
        "op",
        r#"if has account list; then cat "$DIR/accounts.json"; exit 0; fi
if has item list; then cat "$DIR/items.json"; exit 0; fi
if [ "$1" = read ] && has 'op://vaultxyz/itemaaaa/password'; then out 's3cret with spaces '; fi
if [ "$1" = read ]; then cat "$DIR/gone.txt" >&2; exit 1; fi"#,
    )
    .file(
        "accounts.json",
        &json!([{ "url": "my.1password.com", "email": "me@example.com" }]).to_string(),
    )
    .file(
        "items.json",
        &json!([
            {
                "id": "itemaaaa", "title": "Core router", "category": "LOGIN",
                "vault": { "id": "vaultxyz", "name": "Network" },
                "additional_information": "admin",
                "urls": [{ "href": "https://other" }, { "primary": true, "href": "ssh://core1" }]
            },
            {
                "id": "itembbbb", "title": "Wifi", "category": "PASSWORD",
                "vault": { "id": "vaultxyz", "name": "Network" },
                "additional_information": "—"
            },
            { "title": "No id", "vault": { "id": "vaultxyz" } },
        ])
        .to_string(),
    )
    .file(
        "gone.txt",
        "[ERROR] 2026/01/02 03:04:05 \"itemzzzz\" isn't an item in the \"vaultxyz\" vault\n",
    )
}

#[tokio::test]
async fn onepassword_lists_items_and_reads_a_password_by_vault_and_item_id() {
    let op = op();
    let provider = OnePasswordProvider::new(
        &PasswordManagerConfig {
            account: Some("my.1password.com".into()),
            ..op.config()
        },
        &host().without_var("OP_SERVICE_ACCOUNT_TOKEN"),
    );
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [
            entry(
                "vaultxyz/itemaaaa",
                "Core router",
                Some("admin"),
                Some("ssh://core1"),
                Some("Network")
            ),
            entry("vaultxyz/itembbbb", "Wifi", None, None, Some("Network")),
        ]
    );
    // Exactly what op printed: --no-newline leaves nothing to trim.
    assert_eq!(
        provider.password("vaultxyz/itemaaaa").await.unwrap(),
        "s3cret with spaces "
    );
    assert_eq!(
        op.calls().last().unwrap(),
        &[
            "read",
            "--no-newline",
            "op://vaultxyz/itemaaaa/password",
            "--account",
            "my.1password.com"
        ]
    );
}

#[tokio::test]
async fn onepassword_reports_a_removed_item_and_refuses_a_reference_that_is_not_two_ids() {
    let op = op();
    let provider = OnePasswordProvider::new(&op.config(), &host());
    let gone = provider.password("vaultxyz/itemzzzz").await.unwrap_err();
    assert_eq!(gone.problem, PasswordManagerProblem::EntryMissing);
    assert_eq!(gone.message, "The item is no longer in 1Password.");
    let runs = op.calls().len();
    for reference in [
        "vault/item/extra",
        "../x/y?attribute=otp",
        "vaultxyz",
        "/item",
        "vault/",
    ] {
        assert_eq!(
            problem_of(provider.password(reference).await),
            Some(PasswordManagerProblem::EntryMissing),
            "{reference}"
        );
    }
    // None of them reached the program.
    assert_eq!(op.calls().len(), runs);
}

#[tokio::test]
async fn onepassword_says_so_when_no_account_is_available_to_the_cli() {
    let empty = Fake::new("op-empty", "if has account list; then out '[]'; fi");
    let signed_out = OnePasswordProvider::new(
        &empty.config(),
        &host().without_var("OP_SERVICE_ACCOUNT_TOKEN"),
    );
    assert_eq!(
        signed_out.status().await.unwrap().state,
        PasswordManagerState::SignedOut
    );
    // A service account needs no app to approve it, and no run to find that out.
    let token = OnePasswordProvider::new(
        &empty.config(),
        &host().with_var("OP_SERVICE_ACCOUNT_TOKEN", "ops_token"),
    );
    let runs = empty.calls().len();
    assert_eq!(token.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(empty.calls().len(), runs);
    // A program that is not installed is reported as such.
    let missing = OnePasswordProvider::new(
        &PasswordManagerConfig {
            program: Some("/nonexistent/op".into()),
            ..Default::default()
        },
        &host().without_var("OP_SERVICE_ACCOUNT_TOKEN"),
    );
    assert_eq!(
        problem_of(missing.status().await),
        Some(PasswordManagerProblem::Unavailable)
    );
}

// --- Bitwarden -----------------------------------------------------------------------------

const BW_ID: &str = "0a1b2c3d-1111-2222-3333-444455556666";

fn bw() -> Fake {
    Fake::new(
        "bw",
        &format!(
            r#"session="$BW_SESSION"
if [ "$1" = status ]; then
  if [ "$session" = session-key ]; then out '{{"status":"unlocked"}}'; fi
  out '{{"status":"locked"}}'
fi
if [ "$1" = unlock ]; then
  has --raw --nointeraction --passwordenv || fail 'bad unlock arguments' 64
  [ -z "$session" ] || fail 'unlock inherited a session' 64
  name=''; previous=''
  for arg in "$@"; do
    [ "$previous" = --passwordenv ] && name="$arg"
    previous="$arg"
  done
  [ "$(printenv "$name")" = bw-master ] || fail 'Invalid master password.'
  out session-key
fi
if [ "$1" = lock ]; then out 'Your vault is locked.'; fi
[ "$session" = session-key ] || fail 'Vault is locked.'
if has list items; then cat "$DIR/items.json"; exit 0; fi
if has get password {BW_ID}; then printf 'bw-password\n'; exit 0; fi
if has get password; then fail 'Not found.'; fi"#
        ),
    )
    .file(
        "items.json",
        &json!([
            {
                "id": BW_ID, "type": 1, "name": "edge-fw",
                "login": { "username": "fw-admin", "password": "never-kept", "uris": [{ "uri": null }, { "uri": "https://fw" }] }
            },
            { "id": "note", "type": 2, "name": "A note" },
            { "id": "empty", "type": 1, "name": "No password", "login": { "username": "x", "password": null } },
        ])
        .to_string(),
    )
}

#[tokio::test]
async fn bitwarden_unlocks_with_the_master_password_and_keeps_the_session_key_out_of_the_arguments()
{
    let bw = bw();
    let provider = BitwardenProvider::new(&bw.config(), &host().without_var("BW_SESSION"));
    assert!(provider.can_unlock());
    assert_eq!(
        provider.status().await.unwrap().state,
        PasswordManagerState::Locked
    );
    assert_eq!(
        problem_of(provider.entries().await),
        Some(PasswordManagerProblem::Locked)
    );
    let wrong = provider.unlock("wrong").await.unwrap_err();
    assert_eq!(wrong.problem, PasswordManagerProblem::InvalidUnlock);
    assert_eq!(wrong.message, "The Bitwarden master password is wrong.");

    provider.unlock("bw-master").await.unwrap();
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [entry(
            BW_ID,
            "edge-fw",
            Some("fw-admin"),
            Some("https://fw"),
            None
        )]
    );
    assert_eq!(provider.password(BW_ID).await.unwrap(), "bw-password");
    assert_eq!(
        problem_of(
            provider
                .password("0a1b2c3d-0000-0000-0000-000000000000")
                .await
        ),
        Some(PasswordManagerProblem::EntryMissing)
    );
    // A search term instead of an ID would let bw pick an item itself.
    let runs = bw.calls().len();
    assert_eq!(
        problem_of(provider.password("edge-fw").await),
        Some(PasswordManagerProblem::EntryMissing)
    );
    assert_eq!(bw.calls().len(), runs);

    let said: Vec<String> = bw.calls().into_iter().flatten().collect();
    assert!(!said.iter().any(|arg| arg == "session-key"), "{said:?}");
    assert!(!said.iter().any(|arg| arg == "bw-master"), "{said:?}");
    // Every run says it has nobody to ask.
    assert!(
        bw.calls()
            .iter()
            .all(|call| call.iter().any(|arg| arg == "--nointeraction"))
    );

    provider.lock().await.unwrap();
    assert_eq!(
        provider.status().await.unwrap().state,
        PasswordManagerState::Locked
    );
    assert_eq!(
        problem_of(provider.password(BW_ID).await),
        Some(PasswordManagerProblem::Locked)
    );
}

#[tokio::test]
async fn bitwarden_leaves_a_session_it_inherited_in_place() {
    // `bw unlock` done in the shell Muxus was started from.
    let bw = bw();
    let provider =
        BitwardenProvider::new(&bw.config(), &host().with_var("BW_SESSION", "session-key"));
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(provider.password(BW_ID).await.unwrap(), "bw-password");
}

// --- LastPass ------------------------------------------------------------------------------

fn lpass() -> Fake {
    Fake::new(
        "lpass",
        r#"F=$(printf '\037')
if [ "$1" = status ]; then
  [ -n "$FAKE_LPASS_OUT" ] && exit 1
  exit 0
fi
[ "$LPASS_DISABLE_PINENTRY" = 1 ] || fail 'pinentry would have opened' 70
if [ "$1" = ls ]; then
  has "--format=%ai${F}%an${F}%au${F}%al${F}%ag" || fail 'bad format' 64
  printf '4001\037Network\037\037http://group\037\n'
  printf '4002\037core-sw\037netops\037ssh://core-sw\037Network\r\n'
  printf '4003\037A note\037\037http://sn\037\n'
  printf '4004\037no url\037me\037http://\037\n'
  printf 'Network/\037folder line\037\037\037\n'
  exit 0
fi
if has show --password 4002; then printf 'lp-password\n'; exit 0; fi
if [ "$1" = show ]; then
  printf '\033[1;31mError\033[0m: Could not find specified account(s).\n' >&2
  exit 1
fi"#,
    )
}

#[tokio::test]
async fn lastpass_lists_entries_without_folders_and_notes_and_reads_by_numeric_id() {
    let lpass = lpass();
    let provider = LastPassProvider::new(&lpass.config(), &host());
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [
            entry("4004", "no url", Some("me"), None, None),
            entry(
                "4002",
                "core-sw",
                Some("netops"),
                Some("ssh://core-sw"),
                Some("Network")
            ),
        ]
    );
    assert_eq!(provider.password("4002").await.unwrap(), "lp-password");
    let gone = provider.password("4999").await.unwrap_err();
    assert_eq!(gone.problem, PasswordManagerProblem::EntryMissing);
    assert_eq!(gone.message, "The entry is no longer in LastPass.");
    assert_eq!(
        problem_of(provider.password("core-sw").await),
        Some(PasswordManagerProblem::EntryMissing)
    );
}

#[tokio::test]
async fn lastpass_reports_a_cli_that_is_not_logged_in() {
    let lpass = lpass();
    let provider = LastPassProvider::new(&lpass.config(), &host().with_var("FAKE_LPASS_OUT", "1"));
    let status = provider.status().await.unwrap();
    assert_eq!(status.state, PasswordManagerState::SignedOut);
    assert_eq!(
        status.message.as_deref(),
        Some("The LastPass CLI is not logged in. Run “lpass login <email>” in a terminal first.")
    );
}

// --- Dashlane ------------------------------------------------------------------------------

const DL_ID: &str = "AD145B53-B987-4CFE-9408-F25803DC47A4";

fn dcli() -> Fake {
    Fake::new(
        "dcli",
        &format!(
            r#"if [ "$1" = status ]; then printf 'Logged in: Yes\nLogin: me@example.com\nLocked: No\n'; exit 0; fi
if has password --output json; then cat "$DIR/credentials.json"; exit 0; fi
if [ "$1" = read ] && [ "$2" = 'dl://{DL_ID}/password' ]; then printf 'dl-password\n'; exit 0; fi
if [ "$1" = read ]; then fail 'No matching secret found'; fi"#
        ),
    )
    .file(
        "credentials.json",
        &json!([
            { "id": format!("{{{DL_ID}}}"), "title": "lab-router", "login": "labadmin", "password": "never-kept", "url": "https://lab" },
            { "id": "{11111111-2222-3333-4444-555555555555}", "title": "", "email": "me@example.com", "url": "https://mail" },
            { "title": "No id" },
        ])
        .to_string(),
    )
}

#[tokio::test]
async fn dashlane_lists_credentials_and_reads_one_by_id_without_its_braces() {
    let dcli = dcli();
    let provider = DashlaneProvider::new(&dcli.config(), &host());
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [
            entry(
                "11111111-2222-3333-4444-555555555555",
                "https://mail",
                Some("me@example.com"),
                Some("https://mail"),
                None
            ),
            entry(
                DL_ID,
                "lab-router",
                Some("labadmin"),
                Some("https://lab"),
                None
            ),
        ]
    );
    assert_eq!(provider.password(DL_ID).await.unwrap(), "dl-password");
    assert_eq!(
        problem_of(
            provider
                .password("11111111-2222-3333-4444-555555555555")
                .await
        ),
        Some(PasswordManagerProblem::EntryMissing)
    );
    // A title would make dcli return the first match.
    assert_eq!(
        problem_of(provider.password("lab-router").await),
        Some(PasswordManagerProblem::EntryMissing)
    );
}

#[tokio::test]
async fn dashlane_reports_a_cli_that_is_logged_out_or_locked() {
    let logged_out = Fake::new(
        "dcli",
        "if [ \"$1\" = status ]; then out 'Logged in: No'; fi",
    );
    let status = DashlaneProvider::new(&logged_out.config(), &host())
        .status()
        .await
        .unwrap();
    assert_eq!(status.state, PasswordManagerState::SignedOut);
    let locked = Fake::new(
        "dcli",
        "if [ \"$1\" = status ]; then printf 'Logged in: Yes\\nLocked:  Yes\\n'; exit 0; fi",
    );
    let status = DashlaneProvider::new(&locked.config(), &host())
        .status()
        .await
        .unwrap();
    assert_eq!(status.state, PasswordManagerState::SignedOut);
    assert!(status.message.unwrap().contains("locked"));
    // Older versions have no status command; reading an entry then says what is wrong.
    let old = Fake::new("dcli", "");
    assert_eq!(
        DashlaneProvider::new(&old.config(), &host())
            .status()
            .await
            .unwrap(),
        ProviderStatus::ready()
    );
}

// --- Proton Pass ---------------------------------------------------------------------------

fn pass_cli() -> Fake {
    Fake::new(
        "pass-cli",
        r#"if [ "$1" = info ]; then printf 'Email: me@proton.me\n'; exit 0; fi
if has vault list; then cat "$DIR/vaults.json"; exit 0; fi
if has item list --share-id share1; then cat "$DIR/personal.json"; exit 0; fi
if has item list --vault-name 'Work Stuff'; then cat "$DIR/work.json"; exit 0; fi
if has item view --share-id share1 --item-id item1 --field password; then printf 'pp-password\n'; exit 0; fi
if has item view --vault-name 'Work Stuff' --item-id item3; then printf 'by-name-password\n'; exit 0; fi
if has item view; then fail 'Item not found'; fi"#,
    )
    .file(
        "vaults.json",
        &json!({ "vaults": [{ "name": "Personal", "share_id": "share1" }, { "name": "Work Stuff" }] })
            .to_string(),
    )
    .file(
        "personal.json",
        &json!({ "folders": [], "items": [
            { "id": "item1", "content": { "title": "jump host", "content": { "Login": { "username": "", "email": "ops@example.com", "urls": ["ssh://jump"] } } } },
        ] })
        .to_string(),
    )
    .file(
        "work.json",
        &json!({ "items": [
            { "id": "item2", "share_id": "share2", "content": { "title": "db", "content": { "Login": { "username": "dba" } } } },
            { "id": "item3", "content": { "title": "by name", "content": { "Login": { "username": "u" } } } },
        ] })
        .to_string(),
    )
}

#[tokio::test]
async fn proton_pass_lists_logins_vault_by_vault_and_reads_by_share_and_item_id() {
    let pass_cli = pass_cli();
    let provider = ProtonPassProvider::new(&pass_cli.config(), &host());
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [
            entry(
                "share:share1/item1",
                "jump host",
                Some("ops@example.com"),
                Some("ssh://jump"),
                Some("Personal")
            ),
            entry(
                "vault:Work%20Stuff/item3",
                "by name",
                Some("u"),
                None,
                Some("Work Stuff")
            ),
            entry(
                "share:share2/item2",
                "db",
                Some("dba"),
                None,
                Some("Work Stuff")
            ),
        ]
    );
    assert_eq!(
        provider.password("share:share1/item1").await.unwrap(),
        "pp-password"
    );
    assert_eq!(
        provider.password("vault:Work%20Stuff/item3").await.unwrap(),
        "by-name-password"
    );
    assert_eq!(
        problem_of(provider.password("share:share1/gone").await),
        Some(PasswordManagerProblem::EntryMissing)
    );
    let runs = pass_cli.calls().len();
    for reference in ["item1", "vault:%FF%FE/item3", "share:a/b/c"] {
        assert_eq!(
            problem_of(provider.password(reference).await),
            Some(PasswordManagerProblem::EntryMissing),
            "{reference}"
        );
    }
    assert_eq!(pass_cli.calls().len(), runs);
    assert!(pass_cli.calls().iter().any(|call| {
        call.iter().any(|arg| arg == "--filter-type") && call.iter().any(|arg| arg == "login")
    }));
}

#[tokio::test]
async fn proton_pass_reports_a_cli_that_is_not_logged_in() {
    let logged_out = Fake::new("pass-cli", "fail 'Error: session expired, please log in'");
    let provider = ProtonPassProvider::new(&logged_out.config(), &host());
    assert_eq!(
        provider.status().await.unwrap().state,
        PasswordManagerState::SignedOut
    );
    let error = provider.entries().await.unwrap_err();
    assert_eq!(error.problem, PasswordManagerProblem::SignedOut);
    assert_eq!(
        error.message,
        "The Proton Pass CLI is not logged in. Run “pass-cli login” in a terminal first."
    );
}

// --- pass ----------------------------------------------------------------------------------

fn gpg_available() -> bool {
    std::process::Command::new("gpg")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A key ring of its own, with one key that has no passphrase.
struct KeyRing {
    home: tempfile::TempDir,
    fingerprint: String,
}

impl KeyRing {
    fn new() -> Self {
        // A short path: gpg-agent's socket lives under GNUPGHOME and has a length limit.
        let home = tempfile::Builder::new()
            .prefix("mxg-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut ring = Self {
            home,
            fingerprint: String::new(),
        };
        ring.gpg(
            &[
                "--passphrase",
                "",
                "--pinentry-mode",
                "loopback",
                "--quick-generate-key",
                "Muxus Test <muxus@example.invalid>",
                "ed25519",
                "cert,sign",
                "never",
            ],
            None,
        );
        let keys = ring.gpg(&["--list-keys", "--with-colons"], None);
        ring.fingerprint = String::from_utf8_lossy(&keys)
            .lines()
            .find_map(|line| line.strip_prefix("fpr:"))
            .map(|rest| rest.trim_matches(':').to_owned())
            .expect("the key that was just made");
        let fingerprint = ring.fingerprint.clone();
        ring.gpg(
            &[
                "--passphrase",
                "",
                "--pinentry-mode",
                "loopback",
                "--quick-add-key",
                &fingerprint,
                "cv25519",
                "encr",
                "never",
            ],
            None,
        );
        ring
    }

    fn gpg(&self, args: &[&str], input: Option<&str>) -> Vec<u8> {
        use std::io::Write;
        let mut child = std::process::Command::new("gpg")
            .args(["--batch", "--quiet", "--yes"])
            .args(args)
            .env("GNUPGHOME", self.home.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "gpg {args:?}");
        output.stdout
    }

    fn encrypt(&self, store: &Path, name: &str, content: &str) {
        let file = store.join(format!("{name}.gpg"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        self.gpg(
            &[
                "--trust-model",
                "always",
                "--recipient",
                &self.fingerprint,
                "--output",
                &file.to_string_lossy(),
                "--encrypt",
            ],
            Some(content),
        );
    }

    fn host(&self) -> HostEnv {
        HostEnv::system().with_var("GNUPGHOME", self.home.path().to_string_lossy())
    }
}

impl Drop for KeyRing {
    fn drop(&mut self) {
        // No agent is left running for a key ring that is about to go.
        let _ = std::process::Command::new("gpgconf")
            .args(["--kill", "all"])
            .env("GNUPGHOME", self.home.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn pass_config(store: &Path) -> PasswordManagerConfig {
    PasswordManagerConfig {
        path: Some(store.to_string_lossy().into_owned()),
        ..Default::default()
    }
}

#[tokio::test]
async fn pass_lists_the_store_and_decrypts_an_entry_with_gpg() {
    if !gpg_available() {
        eprintln!("skipped: gpg is not installed");
        return;
    }
    let ring = KeyRing::new();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("password-store");
    std::fs::create_dir_all(store.join("servers/lab")).unwrap();
    std::fs::create_dir_all(store.join(".git")).unwrap();
    std::fs::write(store.join(".gpg-id"), format!("{}\n", ring.fingerprint)).unwrap();
    std::fs::write(store.join(".git/ignored.gpg"), "not an entry").unwrap();
    std::fs::write(store.join("servers/README.md"), "not an entry either").unwrap();
    ring.encrypt(
        &store,
        "servers/lab/root",
        "pass-password\nuser: root\nurl: ssh://lab\n",
    );
    ring.encrypt(&store, "wifi", "wifi-password");
    ring.encrypt(&store, "windows", "crlf-password\r\nnotes\r\n");
    std::fs::write(tmp.path().join("outside.gpg"), "outside the store").unwrap();

    let provider = PassProvider::new(&pass_config(&store), &ring.host());
    assert_eq!(provider.status().await.unwrap(), ProviderStatus::ready());
    assert_eq!(
        provider.entries().await.unwrap(),
        [
            entry("wifi", "wifi", None, None, None),
            entry("windows", "windows", None, None, None),
            entry("servers/lab/root", "root", None, None, Some("servers/lab")),
        ]
    );
    // Only the first line is the password.
    assert_eq!(
        provider.password("servers/lab/root").await.unwrap(),
        "pass-password"
    );
    assert_eq!(provider.password("wifi").await.unwrap(), "wifi-password");
    assert_eq!(provider.password("windows").await.unwrap(), "crlf-password");
    assert_eq!(
        problem_of(provider.password("servers/lab/gone").await),
        Some(PasswordManagerProblem::EntryMissing)
    );
    // Nothing outside the store, and nothing hidden inside it.
    assert_eq!(
        problem_of(provider.password("../outside").await),
        Some(PasswordManagerProblem::EntryMissing)
    );
    assert_eq!(
        problem_of(provider.password(".git/ignored").await),
        Some(PasswordManagerProblem::EntryMissing)
    );

    // A file gpg cannot decrypt says what gpg said.
    std::fs::write(store.join("broken.gpg"), "not encrypted").unwrap();
    let broken = provider.password("broken").await.unwrap_err();
    assert_eq!(broken.problem, PasswordManagerProblem::Failed);
    assert!(
        broken
            .message
            .starts_with("gpg could not decrypt “broken”: "),
        "{}",
        broken.message
    );
}

#[tokio::test]
async fn pass_reports_a_store_that_does_not_exist_and_a_missing_gpg() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = PassProvider::new(&pass_config(&tmp.path().join("no-store")), &host());
    let status = provider.status().await.unwrap();
    assert_eq!(status.state, PasswordManagerState::Unavailable);
    assert_eq!(
        status.message.unwrap(),
        format!(
            "The password store {} was not found.",
            tmp.path().join("no-store").display()
        )
    );
    assert_eq!(
        problem_of(provider.entries().await),
        Some(PasswordManagerProblem::Unavailable)
    );

    // A store without GnuPG to read it.
    let store = tmp.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("wifi.gpg"), "encrypted").unwrap();
    let no_gpg = PassProvider::new(&pass_config(&store), &host().with_var("PATH", ""));
    let status = no_gpg.status().await.unwrap();
    assert_eq!(status.state, PasswordManagerState::Unavailable);
    assert_eq!(
        status.message.as_deref(),
        Some("GnuPG (gpg) was not found on this computer.")
    );
    assert_eq!(no_gpg.entries().await.unwrap().len(), 1);
    assert_eq!(
        problem_of(no_gpg.password("wifi").await),
        Some(PasswordManagerProblem::Unavailable)
    );
}

#[tokio::test]
async fn pass_finds_the_store_the_environment_names_or_the_one_in_the_home_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".password-store/work")).unwrap();
    std::fs::write(home.join(".password-store/work/vpn.gpg"), "x").unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("other.gpg"), "x").unwrap();
    let unset = PasswordManagerConfig::default();

    let default_store = host().with_home(&home).without_var("PASSWORD_STORE_DIR");
    assert_eq!(
        PassProvider::new(&unset, &default_store)
            .entries()
            .await
            .unwrap(),
        [entry("work/vpn", "vpn", None, None, Some("work"))]
    );
    let named = host()
        .with_home(&home)
        .with_var("PASSWORD_STORE_DIR", elsewhere.to_string_lossy());
    assert_eq!(
        PassProvider::new(&unset, &named).entries().await.unwrap(),
        [entry("other", "other", None, None, None)]
    );
    // The folder set in Muxus wins over both.
    let configured = PasswordManagerConfig {
        path: Some("~/.password-store/work".into()),
        ..Default::default()
    };
    assert_eq!(
        PassProvider::new(&configured, &named)
            .entries()
            .await
            .unwrap(),
        [entry("vpn", "vpn", None, None, None)]
    );
}
