//! The HTTP routes of the TypeScript server (`server/src/routes`), as [`crate::Backend`]
//! methods with the same validation, errors and side effects. Request and response types are
//! the `muxus_core::api` DTOs the routes exchanged as JSON; where a route answered with a bare
//! value (`{ deleted: true }`), the method returns the value.
//!
//! | TypeScript route file | Module |
//! |---|---|
//! | `app.ts`, `logs.ts`, `local-shells.ts`, `serial.ts` | [`app`] |
//! | `diagnostics.ts`, `host-stats.ts` | [`diagnostics`] |
//! | `folders.ts` | [`folders`] |
//! | `forwards.ts`, `tunnels.ts` | [`forwards`] |
//! | `profiles.ts`, `host-order.ts`, `terminal-snapshots.ts`, `metadata-schema.ts` | [`hosts`] |
//! | `local-files.ts` | [`local_files`] |
//! | `password-vault.ts` | [`vault`] |
//! | `session-history.ts` | [`history`] |
//! | `sftp.ts`, `ws/sftp-lease-socket.ts` | [`sftp`] |
//! | `ssh.ts` | [`ssh`] |
//! | `workspaces.ts` | [`workspaces`] |
//! | `management-requests.ts` | not here: see [`crate::management`] |

pub mod app;
pub mod diagnostics;
pub mod folders;
pub mod forwards;
pub mod history;
pub mod hosts;
pub mod local_files;
pub mod sftp;
pub mod ssh;
pub mod vault;
pub mod workspaces;
