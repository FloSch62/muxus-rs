//! The one icon per connection kind, shared by every host listing. Port of
//! `client/src/components/host-kind-icon.ts`; names follow the
//! `@mui/icons-material` components (see `folder_icons`).

use crate::model::{ProfileKind, SessionProfile};

pub fn host_kind_icon_name(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Telnet => "LanguageOutlined",
        ProfileKind::Serial => "UsbOutlined",
        ProfileKind::Rdp => "DesktopWindowsOutlined",
        ProfileKind::Vnc => "ScreenShareOutlined",
        ProfileKind::Gnmi => "SensorsOutlined",
        ProfileKind::Netconf => "AccountTreeOutlined",
        ProfileKind::Web => "PublicOutlined",
        ProfileKind::Ssh | ProfileKind::Local => "DnsOutlined",
    }
}

/// Tabs into a container, pod or VM console show what they entered, not how
/// (`environmentTabIcon`).
pub fn environment_tab_icon(profile: &SessionProfile) -> Option<&'static str> {
    use muxus_core::environments::EnvironmentTarget;
    let target = match profile {
        SessionProfile::Local(local) => local.environment.as_ref(),
        SessionProfile::Ssh(ssh) => ssh.environment.as_ref(),
        _ => None,
    }?;
    Some(match target {
        EnvironmentTarget::Container { .. } => "ViewInArOutlined",
        EnvironmentTarget::Pod { .. } => "WidgetsOutlined",
        EnvironmentTarget::ContainerLogs { .. } | EnvironmentTarget::PodLogs { .. } => {
            "SubjectOutlined"
        }
        _ => "ComputerOutlined",
    })
}
