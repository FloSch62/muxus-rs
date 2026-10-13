//! The one icon per connection kind, shared by every host listing. Port of
//! `client/src/components/host-kind-icon.ts`; names follow the
//! `@mui/icons-material` components (see `folder_icons`).

use crate::model::ProfileKind;

pub fn host_kind_icon_name(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Telnet => "LanguageOutlined",
        ProfileKind::Serial => "UsbOutlined",
        ProfileKind::Rdp => "DesktopWindowsOutlined",
        ProfileKind::Vnc => "ScreenShareOutlined",
        ProfileKind::Gnmi => "SensorsOutlined",
        ProfileKind::Netconf => "AccountTreeOutlined",
        ProfileKind::Ssh | ProfileKind::Local => "DnsOutlined",
    }
}
