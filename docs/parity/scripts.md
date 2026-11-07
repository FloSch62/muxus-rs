# Parity: scripts on many hosts

Upstream: 75f767a (run a script on many SSH hosts), 4ec05d2 (stoppable, robust runs),
2268f79 (TMPDIR that points nowhere). Guide: `../muxus/docs/guide/scripts.md`.

The engine lives in `crates/muxus-session/src/scripts/` (`Backend::start_script_job` and the
script routes); the dialog, the window's job and the openers in
`crates/muxus-app/src/features/scripts/`, wired into the window by `src/ui/scripts.rs`.

```bash
cargo test -p muxus-session --test scripts       # runs and jobs against the test sshd
cargo test -p muxus-session --lib scripts        # run plan, wrapper
cargo test -p muxus-app --lib scripts            # targets, job model, form checks
cargo test -p muxus-app --test ui_scripts        # the dialog, end to end
```

The tests run each exec with the local `sh` through the test sshd's `exec_shell` mode
(`muxus_ssh::testing`), each in a process group of its own as sshd does, so cancelling and the
time limit are tested against real processes.

## Backend

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Script library (`GET/PUT/DELETE /api/scripts`) | server/src/routes/scripts.ts | done | `Backend::{scripts, save_script, delete_script}` over the `scripts` table |
| Start, read and cancel a run on a live connection (`/api/script-runs`) | routes/scripts.ts, scripts/script-runs.ts | done | `Backend::{start_script_run, script_run_status, cancel_script_run}`; offsets count UTF-8 bytes instead of UTF-16 units (in process, nothing else reads them) |
| The sh wrapper: temp file (mktemp, /tmp fallback, exclusive private fallback), `#!` interpreter, empty stdin, exit 125 | scripts/script-runs.ts (`SCRIPT_WRAPPER`) | done | `scripts/wrapper.rs`, byte for byte |
| Job marker on stderr, kept out of the output; stderr held until it comes (64 KiB) | script-runs.ts | done | |
| Cancel and time limit kill the run's process group over the same connection, then TERM and close the channel | script-runs.ts (`killJobCommand`) | done | |
| Run ends on channel close; exit status sent with the exec reply kept; signal exit; connection lost | script-runs.ts | done | |
| "No POSIX shell" error for hosts without sh, any locale | script-runs.ts | done | |
| 1 MB of output kept per run, older dropped; finished runs kept 30 min, at most 1000 | script-runs.ts | done | |
| Runs hold their own `script` lease; stopped on shutdown (`stopAll`) | app.ts, connection-leases.ts | done | `ConnectionLeaseOwner::Script` |
| Run modes: all at once, batches of n, one by one; stop at the first failure | client/src/scripts/run-plan.ts | done | `scripts/plan.rs`; run-plan.test.ts ported |
| Each host dials the way its terminal tab would (fallback routes, keys, vault, multiplexing) | script-jobs.ts (`dialProfile`) | done | a `dial` session per host |
| Sign-in questions named after the host; withdrawn when a dial gives up or the job is cancelled | script-jobs.ts, dial-prompts.ts | done | `ScriptJobEvent::{Prompt, PromptWithdrawn}`; dial-prompts.test.ts's cases are covered by the job tests |
| Cancel: no more hosts start, dials still connecting give up, runs stop; a cancel during run start is not lost | script-jobs.ts | done | |
| Polling every 500 ms | script-jobs.ts | n/a | the job waits on the run's change signal instead |

## Window

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Scripts dialog: saved scripts list, New, From file… (1 MB limit), Save / Save changes / Saved, Delete with confirmation | components/ScriptsDialog.tsx | done | |
| Script editor (monospace, 10–16 rows) with the helper line | ScriptsDialog.tsx | done | |
| Host picker "Run on (n of m SSH hosts)", All SSH hosts, Clear | ScriptsDialog.tsx | done | MUI `Autocomplete multiple` (`muxus_ui::widgets::autocomplete::Autocomplete::multiple`): picked hosts are small chips inside the search field before the input, wrapping onto more rows; a chip's delete icon or Backspace in the empty field removes one, the clear button all; picked hosts leave the list (`filterSelectedOptions`). Not ported: moving between chips with the arrow keys (`focusedItem`) |
| Modes, batch size (digits only, picking batches), stop at the first failure (off for all at once), time limit in minutes (at most a week) | ScriptsDialog.tsx | done | |
| Run button with the reason it is off | ScriptsDialog.tsx | done | `model::run_problem`, tested |
| Unsaved changes ask before switching scripts or closing | ScriptsDialog.tsx | done | |
| Results: counts, mode line, Cancel / Run again on failed hosts, host list with state icons and durations, output following the bottom, copy, error line | ScriptsDialog.tsx | done | |
| Run again on failed hosts reruns the job's own script | script-jobs.ts | done | |
| The job outlives the dialog; reopening shows it | script-jobs.ts (store) | done | one job per window (`WindowFeatures::scripts`) |
| Window-wide prompt host, one question at a time | WindowDialPrompts.tsx | done | the window's prompt queue; questions now draw over feature dialogs |
| Script targets: SSH config hosts and saved SSH hosts; system OpenSSH hosts left out | script-jobs.ts (`scriptTarget`) | done | |
| Host menu "Run script…" / "Run script on n selected hosts…", greyed out for a system OpenSSH host | sidebar/HostContextMenu.tsx | done | |
| Folder menu "Run script on hosts in this folder…" (subfolders included) | sidebar/FolderContextMenu.tsx | done | |
| Notice for hosts left out, or for none to run on | script-jobs.ts (`openScriptsFor`) | done | |
| Selection bar button | sidebar/SelectionBar.tsx | done | `ui/sidebar_selection.rs`: Run script… between Move and Delete opens Scripts with the selected hosts as "The selection"; Scripts leaves out what cannot run one and says so |
| Quick launcher "Run a script on hosts" | QuickLauncherDialog.tsx | done | |
| Keyboard command `app.scripts` | keymap/commands.ts | done | |
