# Shared sessions parity

Port of the shared sessions of Muxus pull request #381 (`feat/shared-sessions`): a session
list published at a web address or kept on a shared drive, whose hosts Muxus adds, changes
and removes inside one sidebar folder, and the other direction, writing a folder as such a
list.

> **The pull request is still open.** It adds migration 39 (`shared-sessions`). A database
> this build has opened is refused by a Muxus build without that migration, and the
> migration may still change before it is merged. `docs/architecture.md` asks that Muxus-rs
> never runs a schema migration Muxus does not know; this port is ahead of that rule until
> #381 lands.

The work is spread over the layers the TypeScript spreads it over:

| Part | Crate | TypeScript |
|---|---|---|
| Types, when a link is due, validation | `muxus-core` (`api/shared_sessions.rs`) | `shared/src/api-types.ts`, the route's zod schemas |
| Links and applying a list | `muxus-store` (`db/shared_sessions.rs`) | `server/src/persistence/database.ts` |
| Reading a list's text, writing a folder as a list | `muxus-import` (`shared_sessions.rs`, `shared_sessions_export.rs`) | `client/src/shared-sessions.ts`, `shared-sessions-export.ts`, `shared-sessions-format.ts`, `folder-defaults.ts` |
| Reading a link or file, the routes | `muxus-session` (`shared_sessions.rs`, `routes/shared_sessions.rs`) | `server/src/shared-sessions/fetch.ts`, `server/src/routes/shared-sessions.ts` |
| Dialogs, sidebar, Settings, the checks while the app runs | `muxus-app` (`features/shared_sessions`, `ui/shared_sessions.rs`) | `client/src/api/shared-sessions.ts`, `SharedSessions*.tsx`, `ShareFolderDialog.tsx`, `sidebar/*` |

```bash
cargo test -p muxus-core shared_session
cargo test -p muxus-store --test db_repositories shared_sessions
cargo test -p muxus-store --test db_ts_fixture
cargo test -p muxus-import --test shared_sessions
cargo test -p muxus-session --test shared_sessions
cargo test -p muxus-app --test ui_shared_sessions
# Against a real web server (nginx: redirects, a Windows code page, https with a company
# authority and with a certificate nobody vouches for):
docker build -t muxusrs-e2e-portal hack/e2e/containers/portal
docker run -d --name muxusrs-e2e-portal -p 127.0.0.1:28080:80 -p 127.0.0.1:28443:443 \
  -p 127.0.0.1:28444:444 muxusrs-e2e-portal
mkdir -p target/e2e-portal && curl -s -o target/e2e-portal/ca.pem http://127.0.0.1:28080/ca.pem
SSL_CERT_FILE=$PWD/target/e2e-portal/ca.pem MUXUS_E2E_PORTAL=127.0.0.1 \
  cargo test -p muxus-session --test shared_sessions_portal -- --ignored
docker rm -f muxusrs-e2e-portal
```

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Migration 39: `shared_session_sources`, `shared_source_id` and `shared_entry_json` on hosts | `database.ts` | done | SQL verbatim; fixtures regenerated from the pull request's branch |
| Links: list by name with host counts, follow, change (another address is read at once, another folder takes the hosts along), remove with or without the hosts | `database.ts` | done | `db::shared_sessions` |
| When a link is due: its interval, a quarter of an hour at most after a failure, never when it is only read on request; one read claimed per interval | `api-types.ts` `sharedSessionSourceDue`, `beginSharedSessionCheck` | done | `shared_session_source_due`, `begin_shared_session_check` |
| Applying a list: new hosts added, missing ones deleted, an unchanged entry left alone, a changed one bringing over only the settings the list changed; a host of another link refused; a host restored from a backup taken over | `database.ts` `applySharedSessions`, `mergedSharedProfile`, `canonicalJson` | done | The stored entry is the same text in both builds (keys in JavaScript's order), so a list the Electron build applied reads as unchanged here: `tests/db_ts_fixture.rs` |
| A folder renamed or moved in the sidebar takes its link along | `routes/folders.ts`, `moveSharedSessionFolders` | done | |
| Where a link points: http and https, no user name or password, `file://`, `~`, full paths, `\\server\share` | `fetch.ts` `sharedSessionsLocation` | done | The `url` crate parses addresses as Node's `URL` does |
| Reading a link: redirects (five at most, never into a file), 10 MB, 20 seconds, Windows-1252 files, the operating system's certificate authorities, errors that name the host or the file and never the link | `fetch.ts` | done | hyper over rustls with the roots of `muxus-rdp` (bundled and the system's) |
| Routes: list, follow or change, remove, fetch, apply | `routes/shared-sessions.ts` | done | `Backend::{shared_session_sources, save_shared_session_source, delete_shared_session_source, fetch_shared_sessions, apply_shared_sessions}` |
| What a list cannot set: ProxyCommand, agent forwarding, remote forwards, host key checking, `useConfig` | `routes/shared-sessions.ts` `withoutLocalEffects` | done | |
| A list with a bad entry, an entry listed twice or no entries is refused and the hosts stay | `routes/shared-sessions.ts` | done | The entry is named in the answer and kept with the link |
| One whole check | `client/src/shared-sessions.ts` `syncSharedSessions` | done | `Backend::sync_shared_sessions`: the importers are a crate the backend can use, so the window asks for a check in one call |
| MobaXterm session files (`[Bookmarks]`), gateways as jump hosts in "Jump hosts" | `shared-sessions.ts` | done | Through the MobaXterm import, as Muxus-owned hosts |
| Muxus session lists (`muxus-shared-sessions` version 1), hand-written ones included | `shared-sessions.ts` | done | |
| Muxus backups (saved hosts and ssh_config hosts, hops and gateways pointed at the hosts the list brought) | `shared-sessions.ts` | done | |
| Host IDs derived from the link and the entry | `shared-sessions.ts` | done | The same IDs as the Electron build derives |
| Sharing a folder: subfolders, jump hosts from outside in "Jump hosts", ssh_config hosts with what they resolve to, folder defaults filled in, user names and key paths optional, nothing that runs on the follower | `shared-sessions-export.ts` | done | `share_folder`, `shared_sessions_filename` |
| Follow or change a link: link or file with Browse, name, folder that follows the name, interval | `SharedSessionsDialog.tsx` | done | `features::shared_sessions::dialog` |
| Share a folder: what is saved, the two choices, what to do next, Save list… | `ShareFolderDialog.tsx` | done | `features::shared_sessions::share`; the file is written through the save dialog |
| Stop following, with the hosts kept on request | `api/shared-sessions.ts` `confirmRemoveSharedSessions` | done | `features::shared_sessions::stop`, one dialog for the window and Settings |
| The checks while the app runs: at startup and every minute for links that are due, changed hosts announced, failures left to the link's row and folder | `SharedSessionsSync.tsx` | done | `store::start`: one loop for the app, since every window shares one backend |
| Sidebar: the + menu (host, folder, link), "Shared sessions link…" in the panel menu, a followed folder's icon, tooltip and failure mark, its menu entries (Refresh, Edit link, Share folder, Stop following in place of Delete folder) | `sidebar/AddMenu.tsx`, `FolderContextMenu.tsx`, `FolderRow.tsx`, `PanelContextMenu.tsx`, `folder-icons.ts`, `SessionSidebar.tsx` | done | |
| Settings → Backup & data → Shared sessions | `SharedSessionsGroup.tsx`, `settings-search.ts` | done | In the settings search index |
| Quick launcher "Follow shared sessions" | `QuickLauncherDialog.tsx` | done | |
| Host editor caption of a host a link keeps up to date | `api/shared-sessions.ts` `useSharedHostCaption` | done | |
| Links in a backup, restored under their own ID | `data-transfer.ts` | done | A link this computer cannot use is left out, its hosts are restored |

## Tests

- `muxus-store`: `tests/db_repositories.rs` (`shared_sessions`: when a link is due, an
  address that changed, listing, folder moves, removal), `tests/db_ts_fixture.rs` (the
  links and their hosts as the TypeScript wrote them, and its applied list read as
  unchanged), unit tests of the stored entry text and the merge.
- `muxus-import`: `tests/shared_sessions.rs`, every case of `shared-sessions.test.ts` and
  `shared-sessions-export.test.ts`.
- `muxus-session`: `tests/shared_sessions.rs`, every route case of
  `shared-session-routes.test.ts` against a web server on this machine, plus whole checks
  through `sync_shared_sessions`; `tests/shared_sessions_portal.rs` (ignored unless asked
  for) against the nginx of `hack/e2e/containers/portal`.
- `muxus-app`: the texts (`features/shared_sessions/logic.rs`), which links the app reads
  (`store.rs`), the backup cases of `data-transfer.test.ts`, and `tests/ui_shared_sessions.rs`
  (the dialogs against a sandboxed backend, and a shared folder followed by a second one).

## Known differences

- **Certificate problems** are named in rustls's words (`UnknownIssuer`, `Expired`,
  `NotValidForName`) where Node names OpenSSL's (`SELF_SIGNED_CERT_IN_CHAIN`,
  `CERT_HAS_EXPIRED`). The sentence around them is the same.
- **A link's origin on Windows.** A drive path is named by its file in the link's row
  (`lab.json`). The Electron build parses `C:\…` as an address and shows "link".
- **Saving a shared folder** asks where to save, as the other exports of this build do; the
  Electron build downloads into the browser's folder.
- **One loop for the app.** The Electron build checks from every window and lets the
  backend drop the duplicates; here the windows share one loop.
