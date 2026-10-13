//! Compiles the vendored OpenConfig and gRPC protos with `protox` (no
//! `protoc` needed): typed prost messages for gNMI, and one descriptor set
//! for everything, which the gNOI/gNSI JSON mapping reads at run time.
//!
//! Protobuf maps decode into hash maps, which forget the order the device
//! sent the entries in; a path's keys (`[name=cpm][type=ipv4]`) must keep
//! it. Every map entry type is therefore compiled as an ordinary repeated
//! message (the wire format is the same), and the names of those types are
//! written to `map_entries.txt` so the JSON mapping still shows them as
//! objects.

use std::env;
use std::error::Error;
use std::path::PathBuf;

use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorSet};

/// Every proto the backend speaks. gNOI and gNSI stay dynamic (their JSON
/// form is driven by the descriptors); only gNMI gets Rust types.
const PROTOS: &[&str] = &[
    "github.com/openconfig/gnmi/proto/gnmi/gnmi.proto",
    "github.com/openconfig/gnmi/proto/gnmi_ext/gnmi_ext.proto",
    "github.com/openconfig/gnoi/types/types.proto",
    "github.com/openconfig/gnoi/common/common.proto",
    "github.com/openconfig/gnoi/system/system.proto",
    "github.com/openconfig/gnoi/file/file.proto",
    "github.com/openconfig/gnoi/healthz/healthz.proto",
    "github.com/openconfig/gnoi/bgp/bgp.proto",
    "github.com/openconfig/gnoi/os/os.proto",
    "github.com/openconfig/gnsi/authz/authz.proto",
    "github.com/openconfig/gnsi/pathz/pathz.proto",
    "github.com/openconfig/gnsi/certz/certz.proto",
    "github.com/openconfig/gnsi/credentialz/credentialz.proto",
    "github.com/openconfig/gnsi/acctz/acctz.proto",
    "grpc/reflection/v1/reflection.proto",
    "grpc/reflection/v1alpha/reflection.proto",
];

const GNMI: &str = "github.com/openconfig/gnmi/proto/gnmi/gnmi.proto";

/// Turn map entry types into plain messages, collecting their full names.
fn unmap(set: &mut FileDescriptorSet, names: &mut Vec<String>) {
    fn visit(scope: &str, message: &mut DescriptorProto, names: &mut Vec<String>) {
        let full = format!("{scope}.{}", message.name());
        if let Some(options) = message.options.as_mut()
            && options.map_entry()
        {
            options.map_entry = None;
            names.push(full.clone());
        }
        for nested in &mut message.nested_type {
            visit(&full, nested, names);
        }
    }
    for file in &mut set.file {
        // The entry types have no source location of their own, which
        // the code generators' doc-comment lookup cannot cope with.
        file.source_code_info = None;
        let package = file.package().to_owned();
        for message in &mut file.message_type {
            visit(&package, message, names);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("proto");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    println!("cargo:rerun-if-changed=proto");

    let mut all = protox::compile(PROTOS, [&root])?;
    let mut map_entries = Vec::new();
    unmap(&mut all, &mut map_entries);
    map_entries.sort();
    map_entries.dedup();
    std::fs::write(out.join("management.fds"), all.encode_to_vec())?;
    std::fs::write(out.join("map_entries.txt"), map_entries.join("\n"))?;

    // gNMI messages for the library.
    let mut gnmi = protox::compile([GNMI], [&root])?;
    unmap(&mut gnmi, &mut Vec::new());
    let lib_out = out.join("lib");
    std::fs::create_dir_all(&lib_out)?;
    prost_build::Config::new()
        .out_dir(&lib_out)
        .compile_fds(gnmi.clone())?;

    // The same messages with tonic server stubs, for the in-process test target.
    let test_out = out.join("test");
    std::fs::create_dir_all(&test_out)?;
    tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .build_transport(false)
        .out_dir(&test_out)
        .compile_fds(gnmi)?;
    Ok(())
}
