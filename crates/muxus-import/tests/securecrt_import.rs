//! Port of `tests/unit/client/securecrt-import.test.ts`.

use muxus_core::api::HostBlockOptions;
use muxus_core::protocol::{
    SavedHostSessionProfile, SerialFlowControl, SerialParity, SerialProfile, StopBits,
};
use muxus_import::{
    ImportedAuthMode, ImportedSession, ImportedSshStorage, MAX_SECURECRT_IMPORT_BYTES,
    PortableHostMetadata, PortableSshHost, parse_securecrt_sessions, securecrt_connections,
};

const EXPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<VanDyke version="3.0">
  <key name="Security"><string name="Passphrase">encrypted-secret</string></key>
  <key name="Sessions">
    <key name="Customers &amp; labs">
      <key name="Europe">
        <key name="Core router">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">SSH2</string>
          <string name="Hostname">router.example.test</string>
          <string name="Username">deploy</string>
          <dword name="[SSH2] Port">2222</dword>
          <string name="SSH2 Authentications V2">keyboard-interactive,password,gssapi</string>
          <string name="Password V2">encrypted-password</string>
          <string name="Identity Filename V2">/private/key/that/must/not/import</string>
        </key>
        <key name="Core router">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">SSH2</string>
          <string name="Hostname">key.example.test</string>
          <string name="Username">root</string>
          <dword name="[SSH2] Port">70000</dword>
          <string name="SSH2 Authentications V2">publickey,password</string>
        </key>
      </key>
      <key name="Bench console">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">Serial</string>
        <string name="Mac Com Port">/dev/cu.usbserial-1234</string>
        <dword name="Mac Baud Rate">9600</dword>
        <dword name="Mac Data Bits">7</dword>
        <dword name="Mac Stop Bits">2</dword>
        <dword name="Mac Parity">2</dword>
        <dword name="Mac CTS Flow">1</dword>
        <dword name="Mac XON Flow">1</dword>
      </key>
      <key name="Empty folder"/>
    </key>
    <key name="Local terminal">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Local Shell</string>
    </key>
    <key name="Broken SSH">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">SSH2</string>
      <string name="Hostname"/>
    </key>
  </key>
  <key name="Files">
    <key name="File 0"><binary name="Contents">private-key-bytes</binary></key>
  </key>
</VanDyke>"#;

#[test]
fn reads_nested_ssh_and_serial_sessions_while_counting_unsupported_entries() {
    let parsed = parse_securecrt_sessions(EXPORT).expect("parsed");

    assert_eq!(parsed.ignored_count, 2);
    let skipped: Vec<_> = parsed
        .skipped_sessions
        .iter()
        .map(|skipped| {
            (
                skipped.name.as_str(),
                skipped.folder.as_deref(),
                skipped.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        skipped,
        [
            (
                "Local terminal",
                None,
                "Protocol “Local Shell” is not supported"
            ),
            ("Broken SSH", None, "SSH session has no hostname"),
        ]
    );
    assert_eq!(parsed.sessions.len(), 3);
    let ImportedSession::Ssh(first) = &parsed.sessions[0] else {
        panic!("not SSH");
    };
    assert_eq!(first.name, "Core router");
    assert_eq!(first.alias, "Core-router");
    assert_eq!(first.host, "router.example.test");
    assert_eq!(first.port, 2222);
    assert_eq!(first.username.as_deref(), Some("deploy"));
    assert_eq!(first.folder.as_deref(), Some("Customers & labs/Europe"));
    assert_eq!(first.auth_mode, ImportedAuthMode::Password);
    let ImportedSession::Ssh(second) = &parsed.sessions[1] else {
        panic!("not SSH");
    };
    assert_eq!(second.alias, "Core-router-2");
    assert_eq!(second.host, "key.example.test");
    assert_eq!(second.port, 22);
    assert_eq!(second.auth_mode, ImportedAuthMode::Key);
    let ImportedSession::Serial(serial) = &parsed.sessions[2] else {
        panic!("not serial");
    };
    assert_eq!(serial.name, "Bench console");
    assert_eq!(serial.path, "/dev/cu.usbserial-1234");
    assert_eq!(serial.baud_rate, 9600);
    assert_eq!(serial.data_bits, 7);
    assert_eq!(serial.stop_bits, StopBits::Two);
    assert_eq!(serial.parity, SerialParity::Even);
    assert_eq!(serial.flow_control, SerialFlowControl::Hardware);
    assert_eq!(serial.folder.as_deref(), Some("Customers & labs"));
    assert!(serial.profile_id.starts_with("securecrt-serial-"));
}

#[test]
fn uses_unprefixed_serial_fields_and_software_flow_control_fallbacks() {
    let parsed = parse_securecrt_sessions(
        r#"
      <VanDyke><key name="Sessions"><key name="Console">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">Serial</string>
        <string name="Com Port">COM4</string>
        <dword name="Baud Rate">115200</dword>
        <dword name="Data Bits">8</dword>
        <dword name="Stop Bits">1</dword>
        <dword name="Parity">4</dword>
        <dword name="XON Flow">1</dword>
      </key></key></VanDyke>
    "#,
    )
    .expect("parsed");

    let ImportedSession::Serial(serial) = &parsed.sessions[0] else {
        panic!("not serial");
    };
    assert_eq!(serial.path, "COM4");
    assert_eq!(serial.baud_rate, 115_200);
    assert_eq!(serial.data_bits, 8);
    assert_eq!(serial.stop_bits, StopBits::OnePointFive);
    assert_eq!(serial.parity, SerialParity::Space);
    assert_eq!(serial.flow_control, SerialFlowControl::Software);
}

#[test]
fn produces_deterministic_serial_identities() {
    let serial_id = || {
        parse_securecrt_sessions(EXPORT)
            .expect("parsed")
            .sessions
            .iter()
            .find(|session| matches!(session, ImportedSession::Serial(_)))
            .map(|session| session.id().to_owned())
    };
    assert_eq!(serial_id(), serial_id());
}

#[test]
fn rejects_dangerous_oversized_and_unrelated_xml() {
    let error = |text: &str| parse_securecrt_sessions(text).unwrap_err().to_string();
    assert!(error("<!DOCTYPE x><VanDyke/>").contains("declarations or entities"));
    assert!(error("<Other/>").contains("valid SecureCRT"));
    assert!(error("<VanDyke/>").contains("Sessions section"));
    assert!(error("<VanDyke><key></VanDyke>").contains("malformed"));
    assert!(error(&"x".repeat(MAX_SECURECRT_IMPORT_BYTES + 1)).contains("20 MB"));
}

#[test]
fn rejects_files_without_a_supported_complete_session() {
    let error = parse_securecrt_sessions(
        r#"
        <VanDyke><key name="Sessions"><key name="Shell">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">Local Shell</string>
        </key></key></VanDyke>
      "#,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("No supported SSH or serial sessions")
    );
}

#[test]
fn maps_reviewed_sessions_without_copying_secrets_or_file_paths() {
    let parsed = parse_securecrt_sessions(EXPORT).expect("parsed");
    let portable = securecrt_connections(&parsed.sessions, ImportedSshStorage::default());

    let metadata = PortableHostMetadata {
        display_name: Some("Core router".to_owned()),
        group: Some("Customers & labs/Europe".to_owned()),
        ..PortableHostMetadata::default()
    };
    assert_eq!(
        portable.ssh_hosts,
        [
            PortableSshHost {
                alias: "Core-router".to_owned(),
                aliases: vec!["Core-router".to_owned()],
                description: Some("Imported from SecureCRT.".to_owned()),
                options: HostBlockOptions {
                    hostname: Some("router.example.test".to_owned()),
                    user: Some("deploy".to_owned()),
                    port: Some(2222),
                    password_only: Some(true),
                    ..HostBlockOptions::default()
                },
                metadata: Some(metadata.clone()),
            },
            PortableSshHost {
                alias: "Core-router-2".to_owned(),
                aliases: vec!["Core-router-2".to_owned()],
                description: Some("Imported from SecureCRT.".to_owned()),
                options: HostBlockOptions {
                    hostname: Some("key.example.test".to_owned()),
                    user: Some("root".to_owned()),
                    ..HostBlockOptions::default()
                },
                metadata: Some(metadata),
            },
        ]
    );
    assert_eq!(portable.saved_hosts.len(), 1);
    let serial = &portable.saved_hosts[0];
    assert!(serial.id.starts_with("securecrt-serial-"));
    assert_eq!(serial.name, "Bench console");
    assert_eq!(
        serial.profile,
        SavedHostSessionProfile::Serial(SerialProfile {
            baud_rate: 9600,
            data_bits: 7,
            stop_bits: StopBits::Two,
            parity: SerialParity::Even,
            flow_control: SerialFlowControl::Hardware,
            ..SerialProfile::new("/dev/cu.usbserial-1234")
        })
    );
    assert_eq!(
        serial.metadata,
        PortableHostMetadata::in_group(Some("Customers & labs"))
    );
    let json = serde_json::to_string(&portable).expect("JSON");
    assert!(!json.contains("encrypted"));
    assert!(!json.contains("/private/key"));
    assert!(!json.contains("private-key-bytes"));
}

#[test]
fn stores_ssh_sessions_alongside_native_serial_profiles_when_requested() {
    let parsed = parse_securecrt_sessions(EXPORT).expect("parsed");
    let portable = securecrt_connections(&parsed.sessions, ImportedSshStorage::Muxus);

    assert!(portable.ssh_hosts.is_empty());
    let router = portable
        .saved_hosts
        .iter()
        .find(|host| host.id.starts_with("securecrt-ssh-") && host.name == "Core router")
        .expect("router");
    let SavedHostSessionProfile::Ssh(profile) = &router.profile else {
        panic!("not SSH");
    };
    assert_eq!(profile.target, "router.example.test");
    assert_eq!(profile.use_config, Some(false));
    assert_eq!(profile.user.as_deref(), Some("deploy"));
    assert_eq!(profile.port, Some(2222));
    assert_eq!(profile.password_only, Some(true));
    assert_eq!(
        router.metadata,
        PortableHostMetadata::in_group(Some("Customers & labs/Europe"))
    );
    assert!(
        portable
            .saved_hosts
            .iter()
            .any(|host| host.id.starts_with("securecrt-serial-")
                && matches!(host.profile, SavedHostSessionProfile::Serial(_)))
    );
}
