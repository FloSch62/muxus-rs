//! The portable connection set (`PortableConnections` in
//! `client/src/data-transfer.ts`): what an import hands to the conflict-safe
//! restore pipeline, and the `connections` half of a Muxus backup file.
//!
//! The JSON shape is the TypeScript's, so a backup written by either build
//! reads in the other.

use muxus_core::api::{
    HostBlockOptions, HostKeywordHighlightConfig, LoginSequence, ManagedHostRef,
    OpenSshMetadataPatch,
};
use muxus_core::js::Patch;
use muxus_core::protocol::SavedHostSessionProfile;
use muxus_core::validate::Validate;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

/// Muxus metadata carried with a portable host: the metadata patch fields as
/// plain values, plus the sidebar position.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableHostMetadata {
    pub display_name: Option<String>,
    pub group: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub terminal_scheme: Option<String>,
    pub terminal_font_color: Option<String>,
    pub terminal_background_color: Option<String>,
    pub keyword_highlights: Option<HostKeywordHighlightConfig>,
    pub command_button_group: Option<String>,
    pub paste_line_delay_ms: Option<u32>,
    pub paste_char_delay_ms: Option<u32>,
    pub disable_sftp: Option<bool>,
    pub console_compatibility: Option<bool>,
    pub login_sequence: Option<LoginSequence>,
    pub sort_order: Option<i64>,
}

impl PortableHostMetadata {
    /// Only a folder.
    pub fn in_group(group: Option<&str>) -> Self {
        Self {
            group: group.map(str::to_owned),
            ..Self::default()
        }
    }

    /// The metadata PATCH a restore sends (`metadataPatch`): every field is
    /// written, so absent values clear what the existing host had. A login
    /// sequence that does not validate is dropped rather than failing the host.
    pub fn to_patch(&self) -> OpenSshMetadataPatch {
        fn patch<T: Clone>(value: &Option<T>) -> Patch<T> {
            Patch::from(value.clone())
        }
        OpenSshMetadataPatch {
            display_name: patch(&self.display_name),
            group: patch(&self.group),
            color: patch(&self.color),
            icon: patch(&self.icon),
            terminal_scheme: patch(&self.terminal_scheme),
            terminal_font_color: patch(&self.terminal_font_color),
            terminal_background_color: patch(&self.terminal_background_color),
            keyword_highlights: patch(&self.keyword_highlights),
            command_button_group: patch(&self.command_button_group),
            paste_line_delay_ms: patch(&self.paste_line_delay_ms),
            paste_char_delay_ms: patch(&self.paste_char_delay_ms),
            disable_sftp: Some(self.disable_sftp.unwrap_or(false)),
            console_compatibility: Some(self.console_compatibility.unwrap_or(false)),
            login_sequence: self
                .login_sequence
                .clone()
                .filter(|sequence| sequence.clone().validate().is_ok())
                .into(),
        }
    }
}

/// One ssh_config Host block to write.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSshHost {
    pub alias: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub options: HostBlockOptions,
    pub metadata: Option<PortableHostMetadata>,
}

/// One saved Muxus host to write, under a deterministic ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableSavedHost {
    pub id: String,
    pub name: String,
    pub profile: SavedHostSessionProfile,
    pub metadata: PortableHostMetadata,
}

/// Hosts for both stores plus, for backups, their complete sidebar order.
/// Imports leave `host_order` empty.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableConnections {
    pub ssh_hosts: Vec<PortableSshHost>,
    pub saved_hosts: Vec<PortableSavedHost>,
    pub host_order: Vec<ManagedHostRef>,
}
