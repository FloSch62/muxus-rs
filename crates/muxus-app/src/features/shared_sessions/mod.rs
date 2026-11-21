//! Shared sessions in the interface: following a session list from a link or a file
//! (`components/SharedSessionsDialog.tsx`), writing a sidebar folder as such a list
//! (`components/ShareFolderDialog.tsx`), and keeping every followed list's hosts up to
//! date while the app runs (`components/SharedSessionsSync.tsx`,
//! `api/shared-sessions.ts`). The backend reads the links and stores their hosts.

pub mod dialog;
pub mod logic;
pub mod share;
pub mod stop;
pub mod store;
