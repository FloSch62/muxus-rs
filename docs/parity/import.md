# Parity: import (`muxus-import`)

`muxus-import` ports Muxus's session imports and command-line launches without
any UI: the MobaXterm and SecureCRT importers, the reviewed-session model and its
conversion into portable connections, the non-drawing half of the import dialog,
the conflict-safe restore (as a plan of store writes), local MobaXterm discovery,
and the desktop executable's argument parsing and target resolution.

```bash
CARGO_TARGET_DIR=target-import cargo test -p muxus-import
CARGO_TARGET_DIR=target-import cargo clippy -p muxus-import --all-targets -- -D warnings
```

The golden fixture `crates/muxus-import/tests/fixtures/golden.json` is written by
the TypeScript itself. To regenerate it (TypeScript checkout at `../muxus`):

```bash
T=$(mktemp -d /tmp/mxi-XXXX)   # tsx needs a short socket path
cd ../muxus/server && TMPDIR=$T ./node_modules/.bin/tsx \
  --tsconfig ../../muxus-rs/crates/muxus-import/tests/fixtures/generate/tsconfig.json \
  ../../muxus-rs/crates/muxus-import/tests/fixtures/generate/gen.ts
```

## Status

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| MobaXterm bookmarks: SSH (0), RDP (4), VNC (5); `[Bookmarks]`/`[Bookmarks_N]` sections, `SubRep` folders, ports, users, key-auth flag | client/src/mobaxterm-import.ts | done | golden: 11 files, parse result and both conversions JSON-equal to the TypeScript |
| MobaXterm private keys with `_ProfileDir_` / `_MyDocuments_` / `_CurrentDrive_` placeholders | mobaxterm-import.ts `mobaXtermPath` | done | |
| MobaXterm SSH gateways, multi-hop (`__PIPE__`), shared hops, readable names, collision-free aliases | mobaxterm-import.ts `parseGateway`, `jumpHostRegistry` | done | hops are `Arc`-shared like the TypeScript's shared objects |
| MobaXterm "Execute command" with `__PTVIRG__`/`__PIPE__`/`__EQUAL__`/`__DIEZE__` and "do not exit" | mobaxterm-import.ts `startupCommand` | done | |
| MobaXterm skipped-session report and size limit (10 MB) | mobaxterm-import.ts | done | |
| SecureCRT XML export: nested folders, SSH2 (host, user, port, auth intent), serial (port, baud, data/stop bits, parity, flow control) | client/src/securecrt-import.ts | done | golden: 11 documents; `roxmltree` instead of `DOMParser` |
| SecureCRT DTD/entity refusal, size limit (20 MB), root/Sessions checks | securecrt-import.ts | done | see "Known differences" for the malformed-XML message |
| Reviewed-session model, stable IDs (FNV-1a per code point, base 36), alias/name/folder cleaning | client/src/session-import.ts | done | golden helper cases, non-BMP included |
| Conversion to portable connections: ssh_config Host blocks or saved Muxus hosts, "<source> jump hosts" folder, `muxus-host:` hops, desktop SSH gateways | session-import.ts `importedConnections` | done | |
| Portable connection types | client/src/data-transfer.ts (`PortableConnections`, `PortableSshHost`, `PortableSavedHost`, `PortableHostMetadata`) | done | types and `metadataPatch` only; backup documents are not part of this crate |
| Conflict-safe restore: keep/replace, added/replaced/kept counts, metadata patch, host order | data-transfer.ts `restoreImportedConnections` / `restoreConnections` | done | returned as a `RestorePlan`; applying it is `muxus-session` work |
| Import dialog model: vendor copy, size check, file decoding, selection, search, storage choice, conflicts, row details, skipped-list paging, toasts | client/src/components/SessionImportDialog.tsx, MobaXtermImportDialog.tsx, SecureCrtImportDialog.tsx | done | drawing belongs to `muxus-app` |
| Reading a chosen file (size first, BOM-stripping decode, parse off the async threads) | SessionImportDialog.tsx `readFile` | done | `ImportReview::read_file` |
| Local MobaXterm discovery: registry through the same PowerShell script, then `%APPDATA%\MobaXterm\MobaXterm.ini` | electron/src/mobaxterm.ts | partial | INI path and the non-Windows refusal are tested; the registry path (PowerShell, 15 s timeout, hidden window) has not run on Windows |
| Command-line arguments: `--host/--folder/--workspace/--connect`, `--user/--port`, link arguments, second-instance payloads, window routing | electron/src/command-line.ts | done | golden: 42 argv, 17 payloads, 30 connect targets |
| `--host`, `--folder`, `--workspace` resolution | client/src/command-line-launch.ts | done | folders use an internal port of the sidebar grouping (`groupHosts`, `groupManagedHosts`, `buildHostTree`) without filters, remembered empty folders or manual folder order, which resolution never uses |
| ssh:// and telnet:// link resolution against saved hosts | command-line-launch.ts `resolveConnectionLink` | done | the link grammar itself is `muxus_core::connection_links` |
| Launch handler decisions and messages | client/src/components/CommandLineLaunchHandler.tsx | done | `plan_command_line_launch`; the request queue, catalog loading and performing the action belong to `muxus-app` |
| `String.prototype.localeCompare` | (implicit, ICU root collation) | partial | ASCII ordering matches the renderer exactly (golden, 67 strings); accents are secondary, `ß` expands; other scripts sort by code point inside their class |
| ssh_config import | — | n/a | Muxus has no ssh_config importer: ssh_config is itself a host store. The OpenSSH *export* lives in data-transfer.ts with backups |
| Vendor colors | — | n/a | the TypeScript maps no colors from MobaXterm or SecureCRT |

## Tests

87 tests, all passing:

- `tests/golden.rs` (5): every importer, conversion, helper, the collation and the
  command-line parser against the TypeScript-recorded fixture.
- `tests/mobaxterm_import.rs` (13): `mobaxterm-import.test.ts`, plus the 10 MB limit.
- `tests/securecrt_import.rs` (7): `securecrt-import.test.ts`.
- `tests/command_line.rs` (18): `tests/unit/electron/command-line.test.ts`.
- `tests/command_line_launch.rs` (9): `command-line-launch.test.ts`, plus the
  launch handler's decisions and messages.
- `tests/connection_link_launch.rs` (11): `connection-link-launch.test.ts`.
- `tests/review.rs` (11): the dialog model, restore plans, file reading.
- Unit tests in `src/` (13): header/section/path parsing, declarations, JS
  `parseInt`, IDs, discovery against a scratch `%APPDATA%`.

## Known differences

- **Malformed SecureCRT XML.** The TypeScript unit test (xmldom with a strict
  error handler) expects "That SecureCRT XML file is malformed."; Chromium's
  `DOMParser` instead returns a document with a `parsererror` element, which the
  running app reports as "That is not a valid SecureCRT XML export.". This port
  follows the unit test.
- **XML parser.** `roxmltree` and `DOMParser` agree on well-formed documents.
  Element names are compared as written (`Element.tagName`), so a prefixed
  `x:VanDyke` is rejected as in the TypeScript.
- **Argument lookup.** The TypeScript looks flags up in plain objects, so an
  argument spelled like an `Object.prototype` member (`constructor`, `toString`)
  is mistaken for a flag. That accident is not reproduced.
- **Collation.** See `localeCompare` above; it only affects the order of hosts a
  `--folder` launch opens and which casing wins between folders that differ only
  in case.

## Public API (for other crates)

- `muxus_import::{parse_mobaxterm_sessions, parse_securecrt_sessions}` →
  `Result<ImportedSessionParseResult, ImportError>`; `ImportedSession`
  (`Ssh` / `Rdp` / `Vnc` / `Serial`), `ImportedJumpHost`, `SkippedImportedSession`.
- `muxus_import::{imported_connections, mobaxterm_connections, securecrt_connections}`
  with `ImportedSshStorage::{Openssh, Muxus}` → `PortableConnections`
  (`PortableSshHost` carries a `HostBlockOptions`, `PortableSavedHost` a
  `SavedHostSessionProfile`; `PortableHostMetadata::to_patch` gives the
  `OpenSshMetadataPatch`).
- `muxus_import::review::{ImportVendor, ImportReview, SkippedPage, session_details,
  session_kind_label, skipped_session_name, searchable_session_values,
  decode_import_file}`; `ImportReview::read_file(vendor, path).await`,
  `ImportReview::detect_local_mobaxterm().await`, `ImportReview::plan(&ExistingHosts)`.
- `muxus_import::restore::{plan_restore, ExistingHosts, ConflictStrategy,
  RestorePlan, RestoreStep, RestoreResult}`: steps are `UpsertSshHost`
  (`HostUpsertRequest` + optional metadata patch), `PutSavedHost`
  (`SavedHostProfileInput` + metadata patch) and `SetHostOrder`, in the order the
  TypeScript sends them.
- `muxus_import::mobaxterm_local::{read_local_mobaxterm_sessions,
  read_local_mobaxterm_sessions_with, LocalDiscovery}` → `MobaXtermSessionSource`.
- `muxus_import::launch::{parse_command_line_launch, parse_command_line_launch_data,
  parse_connect_target, link_launch, can_handle_command_line_launch,
  resolve_command_line_host, resolve_command_line_folder,
  resolve_command_line_workspace, resolve_connection_link, plan_command_line_launch,
  ManagedHost, TargetResolution, ConnectionLinkLaunch, LaunchAction, …}`.
- `muxus_import::locale_compare`.

## Attribution

No third-party code was adapted. Everything here is ported from Muxus itself
(MIT). Dependencies used as libraries: `roxmltree`, `base64`,
`unicode-normalization`, `regex` (all MIT/Apache-2.0).
