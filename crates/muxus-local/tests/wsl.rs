//! Port of `tests/unit/server/wsl.test.ts`.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;

use base64::Engine as _;
use muxus_local::wsl::{
    WslRegistration, list_wsl_distributions_with, parse_wsl_registry, read_wsl_distribution_icon,
};
use muxus_local::{Platform, WslDistribution};

const ROOT: &str = r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Lxss";
const UBUNTU: &str = "{3f4e1a2b-7c5d-4e6f-8a9b-0c1d2e3f4a5b}";
const DEBIAN: &str = "{9a8b7c6d-5e4f-4a3b-9c2d-1e0f9a8b7c6d}";
const DOCKER: &str = "{11111111-2222-4333-8444-555555555555}";
const OTHER: &str = "{22222222-3333-4444-8555-666666666666}";
const UBUNTU_STORE_PATH: &str =
    r"C:\Users\dev\AppData\Local\Packages\CanonicalGroupLimited.Ubuntu24.04LTS\LocalState";

fn debian_path() -> String {
    format!(r"C:\Users\dev\AppData\Local\wsl\{DEBIAN}")
}

/// `reg query <Lxss> /s` as Windows prints it: CRLF lines, four-space columns.
fn reg_output(lines: &[String]) -> String {
    let mut all = vec![String::new()];
    all.extend(lines.iter().cloned());
    all.extend([
        String::new(),
        "End of search: 12 match(es) found.".into(),
        String::new(),
    ]);
    all.join("\r\n")
}

fn installed() -> String {
    reg_output(&[
        ROOT.into(),
        format!(
            "    DefaultDistribution    REG_SZ    {}",
            DEBIAN.to_uppercase()
        ),
        "    DefaultVersion    REG_DWORD    0x2".into(),
        String::new(),
        format!(r"{ROOT}\{UBUNTU}"),
        "    State    REG_DWORD    0x1".into(),
        "    DistributionName    REG_SZ    Ubuntu-24.04".into(),
        "    Version    REG_DWORD    0x2".into(),
        format!("    BasePath    REG_SZ    {UBUNTU_STORE_PATH}"),
        r"    DefaultEnvironment    REG_MULTI_SZ    HOSTTYPE=x86_64\0LANG=en_US.UTF-8".into(),
        String::new(),
        format!(r"{ROOT}\{DOCKER}"),
        "    DistributionName    REG_SZ    docker-desktop".into(),
        String::new(),
        format!(r"{ROOT}\{DEBIAN}"),
        "    DistributionName    REG_SZ    Debian".into(),
        format!("    BasePath    REG_SZ    {}", debian_path()),
        "    Modern    REG_DWORD    0x1".into(),
        String::new(),
        format!(r"{ROOT}\{OTHER}"),
        "    DistributionName    REG_SZ    Ubuntu".into(),
        String::new(),
        format!(r"{ROOT}\{UBUNTU}\Nested"),
        "    DistributionName    REG_SZ    also-not-one".into(),
    ])
}

fn registration(name: &str, is_default: bool, base_path: Option<&str>) -> WslRegistration {
    WslRegistration {
        name: name.into(),
        is_default,
        base_path: base_path.map(str::to_string),
    }
}

fn names(registrations: Vec<WslRegistration>) -> Vec<String> {
    registrations
        .into_iter()
        .map(|registration| registration.name)
        .collect()
}

#[test]
fn lists_every_registered_distribution_the_default_first() {
    assert_eq!(
        parse_wsl_registry(&installed()),
        vec![
            registration("Debian", true, Some(&debian_path())),
            registration("Ubuntu", false, None),
            registration("Ubuntu-24.04", false, Some(UBUNTU_STORE_PATH)),
        ]
    );
}

#[test]
fn hides_the_docker_desktop_and_rancher_desktop_engine_distributions() {
    let output = reg_output(&[
        format!(r"{ROOT}\{DOCKER}"),
        "    DistributionName    REG_SZ    docker-desktop-data".into(),
        format!(r"{ROOT}\{UBUNTU}"),
        "    DistributionName    REG_SZ    rancher-desktop".into(),
        format!(r"{ROOT}\{DEBIAN}"),
        "    DistributionName    REG_SZ    Debian".into(),
    ]);
    assert_eq!(names(parse_wsl_registry(&output)), vec!["Debian"]);
}

#[test]
fn skips_names_that_are_empty_garbled_or_duplicated() {
    let output = reg_output(&[
        format!(r"{ROOT}\{UBUNTU}"),
        "    DistributionName    REG_SZ    ".into(),
        format!(r"{ROOT}\{DEBIAN}"),
        "    DistributionName    REG_SZ    Arch\u{fffd}".into(),
        format!(r"{ROOT}\{DOCKER}"),
        "    DistributionName    REG_SZ    Alpine".into(),
        format!(r"{ROOT}\{OTHER}"),
        "    DistributionName    REG_SZ    alpine".into(),
    ]);
    assert_eq!(names(parse_wsl_registry(&output)), vec!["Alpine"]);
}

#[test]
fn lists_distributions_when_the_lxss_key_has_no_values_of_its_own() {
    let output = reg_output(&[
        format!(r"{ROOT}\{UBUNTU}"),
        "    DistributionName    REG_SZ    Ubuntu".into(),
    ]);
    assert_eq!(
        parse_wsl_registry(&output),
        vec![registration("Ubuntu", false, None)]
    );
}

#[test]
fn never_queries_the_registry_off_windows() {
    let queried = RefCell::new(0);
    for platform in [Platform::Linux, Platform::MacOs] {
        let listed = list_wsl_distributions_with(
            platform,
            || {
                *queried.borrow_mut() += 1;
                Ok(installed())
            },
            |_| None,
        );
        assert!(listed.is_empty());
    }
    assert_eq!(*queried.borrow(), 0);
}

#[test]
fn reports_nothing_when_wsl_has_never_been_set_up() {
    // reg.exe exits 1 when the key does not exist.
    let listed = list_wsl_distributions_with(
        Platform::Windows,
        || Err(io::Error::other("Command failed: reg.exe query")),
        |_| None,
    );
    assert!(listed.is_empty());
}

#[test]
fn parses_the_registry_on_windows() {
    let listed = list_wsl_distributions_with(Platform::Windows, || Ok(installed()), |_| None);
    assert_eq!(
        listed[0],
        WslDistribution {
            name: "Debian".into(),
            is_default: true,
            icon: None
        }
    );
}

#[test]
fn attaches_the_icon_each_distribution_ships_without_its_folder() {
    let asked = RefCell::new(Vec::new());
    let listed = list_wsl_distributions_with(
        Platform::Windows,
        || Ok(installed()),
        |base_path: &Path| {
            asked
                .borrow_mut()
                .push(base_path.to_string_lossy().into_owned());
            (base_path == Path::new(&debian_path()))
                .then(|| "data:image/x-icon;base64,AAABAA==".to_string())
        },
    );
    assert_eq!(
        listed,
        vec![
            WslDistribution {
                name: "Debian".into(),
                is_default: true,
                icon: Some("data:image/x-icon;base64,AAABAA==".into())
            },
            WslDistribution {
                name: "Ubuntu".into(),
                is_default: false,
                icon: None
            },
            WslDistribution {
                name: "Ubuntu-24.04".into(),
                is_default: false,
                icon: None
            },
        ]
    );
    let mut asked = asked.into_inner();
    asked.sort();
    let mut expected = vec![debian_path(), UBUNTU_STORE_PATH.to_string()];
    expected.sort();
    assert_eq!(asked, expected);
    // The folder never leaves the server.
    let json = serde_json::to_value(&listed[1]).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "name": "Ubuntu", "isDefault": false })
    );
}

#[test]
fn reads_the_shortcut_icon_wsl_keeps_in_the_distribution_folder() {
    let folder = tempfile::tempdir().unwrap();
    let icon = [0u8, 0, 1, 0, 1, 0, 16, 16, 0, 0];
    fs::write(folder.path().join("shortcut.ico"), icon).unwrap();
    assert_eq!(
        read_wsl_distribution_icon(folder.path()),
        Some(format!(
            "data:image/x-icon;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(icon)
        ))
    );
}

#[test]
fn has_no_icon_for_a_folder_without_one() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(read_wsl_distribution_icon(folder.path()), None);
    assert_eq!(
        read_wsl_distribution_icon(&folder.path().join("missing")),
        None
    );
}

#[test]
fn ignores_a_file_that_is_not_an_icon_or_far_too_large_to_be_one() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("shortcut.ico"), "<svg/>").unwrap();
    assert_eq!(read_wsl_distribution_icon(folder.path()), None);

    let mut huge = vec![0u8; 512 * 1024];
    huge[..4].copy_from_slice(&[0, 0, 1, 0]);
    fs::write(folder.path().join("shortcut.ico"), huge).unwrap();
    assert_eq!(read_wsl_distribution_icon(folder.path()), None);
}
