//! The operating system a decision is made for.
//!
//! Pure helpers take it as an argument so tests can exercise every platform on any machine; the
//! running build uses [`Platform::current`].

/// An operating system, as Node's `process.platform` tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    FreeBsd,
    /// Another Unix.
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "freebsd") {
            Self::FreeBsd
        } else {
            Self::Other
        }
    }

    /// Node's `process.platform` name (`win32`, `darwin`, `linux`, `freebsd`), which the
    /// renderer keys platform-specific behaviour on.
    pub const fn node_name(self) -> &'static str {
        match self {
            Self::Windows => "win32",
            Self::MacOs => "darwin",
            Self::Linux => "linux",
            Self::FreeBsd => "freebsd",
            Self::Other => {
                if cfg!(target_os = "openbsd") {
                    "openbsd"
                } else if cfg!(target_os = "netbsd") {
                    "netbsd"
                } else if cfg!(target_os = "solaris") || cfg!(target_os = "illumos") {
                    "sunos"
                } else {
                    "linux"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_node() {
        assert_eq!(Platform::Windows.node_name(), "win32");
        assert_eq!(Platform::MacOs.node_name(), "darwin");
        assert_eq!(Platform::Linux.node_name(), "linux");
        if cfg!(target_os = "linux") {
            assert_eq!(Platform::current(), Platform::Linux);
        }
    }
}
