//! Finding the current Windows user's MobaXterm bookmarks (port of
//! `electron/src/mobaxterm.ts`): the installed edition's registry first,
//! then the portable edition's conventional `%APPDATA%\MobaXterm\MobaXterm.ini`.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use muxus_core::api::MobaXtermSessionSource;
use muxus_core::js;

use crate::session::ImportError;

const MAX_MOBAXTERM_FILE_BYTES: usize = 10 * 1024 * 1024;
const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(15);

// Only bookmark values are read. Password and credential registry subkeys are
// deliberately excluded: Muxus prompts for secrets and never persists them.
const READ_BOOKMARKS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$base = Get-Item -LiteralPath 'Registry::HKEY_CURRENT_USER\Software\Mobatek\MobaXterm'
$sections = @(
  Get-ChildItem -LiteralPath $base.PSPath |
    Where-Object { $_.PSChildName -match '^Bookmarks(?:_\d+)?$' } |
    Sort-Object -Property PSChildName
)
if ($sections.Count -eq 0) { exit 2 }
$builder = [System.Text.StringBuilder]::new()
foreach ($section in $sections) {
  $values = Get-ItemProperty -LiteralPath $section.PSPath
  [void]$builder.AppendLine("[$($section.PSChildName)]")
  if ($null -ne $values.SubRep) {
    [void]$builder.AppendLine("SubRep=$($values.SubRep)")
  }
  foreach ($property in $values.PSObject.Properties) {
    if ($property.Name -eq 'SubRep' -or $property.Name -like 'PS*') { continue }
    [void]$builder.AppendLine("$($property.Name)=$($property.Value)")
  }
  [void]$builder.AppendLine()
}
$bytes = [System.Text.Encoding]::UTF8.GetBytes($builder.ToString())
[Console]::Out.Write([Convert]::ToBase64String($bytes))
"#;

/// Where discovery looks; [`LocalDiscovery::system`] is this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDiscovery {
    /// Discovery only exists on Windows.
    pub windows: bool,
    /// `%APPDATA%`.
    pub app_data: Option<PathBuf>,
    /// Ask the registry (through PowerShell) before the portable INI.
    pub read_registry: bool,
}

impl LocalDiscovery {
    pub fn system() -> Self {
        Self {
            windows: cfg!(windows),
            app_data: std::env::var_os("APPDATA").map(PathBuf::from),
            read_registry: cfg!(windows),
        }
    }
}

/// Discover local installer bookmarks first, then the conventional portable INI.
pub async fn read_local_mobaxterm_sessions() -> Result<MobaXtermSessionSource, ImportError> {
    read_local_mobaxterm_sessions_with(&LocalDiscovery::system()).await
}

/// [`read_local_mobaxterm_sessions`] against an explicit environment.
pub async fn read_local_mobaxterm_sessions_with(
    discovery: &LocalDiscovery,
) -> Result<MobaXtermSessionSource, ImportError> {
    if !discovery.windows {
        return Err(ImportError::new(
            "Automatic MobaXterm discovery is only available on Windows.",
        ));
    }
    let registry = if discovery.read_registry {
        read_registry_bookmarks().await
    } else {
        None
    };
    let source = match registry {
        Some(source) => Some(source),
        None => read_portable_ini(discovery.app_data.as_deref()).await,
    };
    source.ok_or_else(|| {
        ImportError::new(
            "No local MobaXterm bookmarks were found. Choose MobaXterm.ini or an .mxtsessions export instead.",
        )
    })
}

async fn read_registry_bookmarks() -> Option<MobaXtermSessionSource> {
    let encoded = run_powershell(READ_BOOKMARKS_SCRIPT).await?;
    let encoded = js::js_trim(&encoded);
    if encoded.is_empty() {
        return None;
    }
    let bytes = decode_base64_leniently(encoded)?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    if js::js_trim(&content).is_empty() || content.len() > MAX_MOBAXTERM_FILE_BYTES {
        return None;
    }
    Some(MobaXtermSessionSource {
        source: "MobaXterm Windows registry".to_owned(),
        content,
    })
}

/// `Buffer.from(text, 'base64')`: stops at the first `=`, skips characters
/// outside the alphabet and accepts the URL-safe one too.
fn decode_base64_leniently(text: &str) -> Option<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let cleaned: String = text
        .split('=')
        .next()
        .unwrap_or("")
        .chars()
        .filter_map(|c| match c {
            '-' => Some('+'),
            '_' => Some('/'),
            c if c.is_ascii_alphanumeric() || c == '+' || c == '/' => Some(c),
            _ => None,
        })
        .collect();
    let engine = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    engine.decode(cleaned).ok()
}

/// Run a PowerShell script hidden, without a profile, and return its
/// standard output; `None` on any failure, a non-zero exit or after 15 s.
async fn run_powershell(script: &str) -> Option<String> {
    let mut command = tokio::process::Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = tokio::time::timeout(POWERSHELL_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_MOBAXTERM_FILE_BYTES * 2 {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn read_portable_ini(app_data: Option<&std::path::Path>) -> Option<MobaXtermSessionSource> {
    let filename = app_data?.join("MobaXterm").join("MobaXterm.ini");
    let bytes = tokio::fs::read(&filename).await.ok()?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    if js::js_trim(&content).is_empty() || content.len() > MAX_MOBAXTERM_FILE_BYTES {
        return None;
    }
    Some(MobaXtermSessionSource {
        source: filename.to_string_lossy().into_owned(),
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    #[test]
    fn discovery_is_windows_only() {
        let discovery = LocalDiscovery {
            windows: false,
            app_data: None,
            read_registry: false,
        };
        let error = runtime()
            .block_on(read_local_mobaxterm_sessions_with(&discovery))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Automatic MobaXterm discovery is only available on Windows."
        );
    }

    #[test]
    fn reads_the_portable_ini_from_app_data() {
        let scratch = tempfile::tempdir().expect("scratch dir");
        let folder = scratch.path().join("MobaXterm");
        std::fs::create_dir_all(&folder).expect("folder");
        let content = "[Bookmarks]\r\nSubRep=\r\nBox=#109#0%box.example.test%22%root%%\r\n";
        std::fs::write(folder.join("MobaXterm.ini"), content).expect("ini");
        let discovery = LocalDiscovery {
            windows: true,
            app_data: Some(scratch.path().to_owned()),
            read_registry: false,
        };
        let source = runtime()
            .block_on(read_local_mobaxterm_sessions_with(&discovery))
            .expect("source");
        assert_eq!(source.content, content);
        assert_eq!(
            source.source,
            folder.join("MobaXterm.ini").to_string_lossy()
        );
    }

    #[test]
    fn reports_missing_and_empty_bookmarks() {
        let scratch = tempfile::tempdir().expect("scratch dir");
        let discovery = LocalDiscovery {
            windows: true,
            app_data: Some(scratch.path().to_owned()),
            read_registry: false,
        };
        let missing = runtime()
            .block_on(read_local_mobaxterm_sessions_with(&discovery))
            .unwrap_err();
        assert!(
            missing
                .to_string()
                .starts_with("No local MobaXterm bookmarks were found.")
        );

        let folder = scratch.path().join("MobaXterm");
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(folder.join("MobaXterm.ini"), " \n ").expect("ini");
        assert!(
            runtime()
                .block_on(read_local_mobaxterm_sessions_with(&discovery))
                .is_err()
        );

        let no_app_data = LocalDiscovery {
            app_data: None,
            ..discovery
        };
        assert!(
            runtime()
                .block_on(read_local_mobaxterm_sessions_with(&no_app_data))
                .is_err()
        );
    }
}
