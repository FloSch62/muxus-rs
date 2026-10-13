//! Connection parameters derived the way the TypeScript client derives them.

/// RDP's display-control limits ([MS-RDPEDISP] 2.2.2.2.1): 200–8192, width even.
pub fn rdp_desktop_size(width: f64, height: f64) -> (u16, u16) {
    let clamp = |value: f64| {
        let value = if value.is_finite() {
            value.floor()
        } else {
            0.0
        };
        value.clamp(200.0, 8192.0) as u16
    };
    (clamp(width) & !1, clamp(height))
}

/// A logon name and the domain it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdpLogon {
    pub username: String,
    pub domain: String,
}

/// Split `DOMAIN\user` the way mstsc does when no domain is configured separately.
pub fn rdp_logon(username: &str, domain: Option<&str>) -> RdpLogon {
    let domain = domain.filter(|domain| !domain.is_empty());
    if domain.is_none()
        && let Some(slash) = username.find('\\')
        && slash > 0
    {
        return RdpLogon {
            domain: username[..slash].to_owned(),
            username: username[slash + 1..].to_owned(),
        };
    }
    RdpLogon {
        username: username.to_owned(),
        domain: domain.unwrap_or_default().to_owned(),
    }
}

/// The desktop size to ask for when the pane is too small to measure (the view's fallback).
pub const FALLBACK_DESKTOP_SIZE: (u16, u16) = (1280, 800);

/// How long the view waits for the pane to settle before asking the server to resize.
pub const RESIZE_SETTLE_MS: u64 = 300;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_the_server_for_sizes_display_control_accepts() {
        assert_eq!(rdp_desktop_size(1153.7, 807.2), (1152, 807));
        assert_eq!(rdp_desktop_size(120.0, 90.0), (200, 200));
        assert_eq!(rdp_desktop_size(9000.0, 9000.0), (8192, 8192));
    }

    #[test]
    fn splits_domain_user_unless_a_domain_is_configured() {
        assert_eq!(
            rdp_logon("CORP\\alice", None),
            RdpLogon {
                username: "alice".into(),
                domain: "CORP".into()
            }
        );
        assert_eq!(
            rdp_logon("CORP\\alice", Some("OTHER")),
            RdpLogon {
                username: "CORP\\alice".into(),
                domain: "OTHER".into()
            }
        );
        assert_eq!(
            rdp_logon("alice@corp.example", None),
            RdpLogon {
                username: "alice@corp.example".into(),
                domain: String::new()
            }
        );
    }
}
