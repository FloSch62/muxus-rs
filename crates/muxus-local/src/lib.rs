//! Local terminal sessions on a pty, shell profiles and launchers.
//!
//! - [`pty`]: start a local shell on a pseudo-terminal and talk to it through
//!   `muxus_core::stream` ([`spawn_local_pty`]), plus the pure launch helpers.
//! - [`shell_integration`]: the OSC 133 rc shims injected into bash and zsh.
//! - [`profile`]: local session profiles and saved shell launch profiles.
//! - [`wsl`]: installed WSL distributions (Windows).
//! - [`login_env`]: the login shell environment a GUI launch should adopt.

pub mod login_env;
pub mod platform;
pub mod profile;
pub mod pty;
pub mod shell_integration;
pub mod wsl;

pub use platform::Platform;
pub use profile::{LocalProfile, LocalShellProfileConfig, WslDistribution};
pub use pty::{DEFAULT_TERM, LocalEnvironment, LocalPty, spawn_local_pty};
