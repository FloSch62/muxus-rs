//! Session imports and command-line launches, without any UI.
//!
//! - [`mobaxterm`] and [`securecrt`] parse the vendors' files into reviewed
//!   sessions ([`session`]); [`mobaxterm_local`] finds the local MobaXterm
//!   bookmarks on Windows.
//! - [`session::imported_connections`] turns reviewed sessions into
//!   [`portable::PortableConnections`]: ssh_config Host blocks or saved Muxus
//!   hosts, with gateways as shared jump hosts.
//! - [`review`] is the import dialog's model (selection, search, conflicts,
//!   messages); [`restore`] plans the conflict-safe writes into the store.
//! - [`launch`] parses the desktop executable's arguments and resolves what
//!   they name against the host and workspace catalogs.
//!
//! Parsing is synchronous and bounded by the vendors' size limits (10 MB and
//! 20 MB). The async entry points ([`ImportReview::read_file`],
//! [`ImportReview::detect_local_mobaxterm`]) run it on tokio's blocking pool;
//! callers that parse text themselves should do the same.

mod collation;
pub mod launch;
pub mod mobaxterm;
pub mod mobaxterm_local;
pub mod portable;
pub mod restore;
pub mod review;
pub mod securecrt;
pub mod session;
mod text;

pub use collation::locale_compare;
pub use mobaxterm::{MAX_MOBAXTERM_IMPORT_BYTES, mobaxterm_connections, parse_mobaxterm_sessions};
pub use portable::{PortableConnections, PortableHostMetadata, PortableSavedHost, PortableSshHost};
pub use restore::{
    ConflictStrategy, ExistingHosts, RestorePlan, RestoreResult, RestoreStep, plan_restore,
};
pub use review::{ImportReview, ImportVendor};
pub use securecrt::{MAX_SECURECRT_IMPORT_BYTES, parse_securecrt_sessions, securecrt_connections};
pub use session::{
    ImportError, ImportedAuthMode, ImportedDesktopSession, ImportedJumpHost, ImportedSerialSession,
    ImportedSession, ImportedSessionKind, ImportedSessionParseResult, ImportedSshSession,
    ImportedSshStorage, SkippedImportedSession, imported_connections,
};
pub use text::{
    clean_import_name, normalize_import_folder, stable_import_id, strip_control_characters,
    unique_import_alias,
};
