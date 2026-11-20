//! External password managers in the interface: the Settings group that connects them
//! (`components/PasswordManagersGroup.tsx`), and the field and picker a host or folder
//! chooses an entry with (`components/password-managers/*`). Every request goes through
//! `muxus_session::Backend`; no view ever holds a password from a manager.

pub mod connect;
pub mod field;
pub mod logic;
pub mod picker;
pub mod store;
pub mod unlock;
