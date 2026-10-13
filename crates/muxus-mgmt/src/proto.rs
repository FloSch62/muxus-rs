//! Generated gNMI messages and the descriptors of every vendored proto.
//!
//! The protos come from openconfig/gnmi, openconfig/gnoi, openconfig/gnsi
//! and grpc/grpc-proto (Apache-2.0, see `proto/LICENSE-APACHE-2.0`). Map
//! fields are compiled as repeated `…Entry` messages so a path keeps its
//! keys in the order the device sent them (see `build.rs`).

use std::collections::HashSet;
use std::sync::LazyLock;

use prost_reflect::DescriptorPool;

#[allow(clippy::all, clippy::pedantic, missing_docs, deprecated)]
pub mod gnmi {
    include!(concat!(env!("OUT_DIR"), "/lib/gnmi.rs"));
}

#[allow(clippy::all, clippy::pedantic, missing_docs, deprecated)]
pub mod gnmi_ext {
    include!(concat!(env!("OUT_DIR"), "/lib/gnmi_ext.rs"));
}

static DESCRIPTORS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/management.fds"));
static MAP_ENTRIES: &str = include_str!(concat!(env!("OUT_DIR"), "/map_entries.txt"));

static POOL: LazyLock<DescriptorPool> = LazyLock::new(|| {
    DescriptorPool::decode(DESCRIPTORS).expect("the vendored protos compile into a valid pool")
});

static MAP_ENTRY_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    MAP_ENTRIES
        .lines()
        .filter(|line| !line.is_empty())
        .collect()
});

/// Descriptors for gNMI, gNOI, gNSI and gRPC reflection.
pub fn pool() -> &'static DescriptorPool {
    &POOL
}

/// Whether a message type stands for the entries of a protobuf map field.
pub fn is_map_entry(full_name: &str) -> bool {
    MAP_ENTRY_NAMES.contains(full_name)
}

/// `{key: value}` pairs as a gNMI path element's ordered key entries.
pub fn path_keys<'a>(
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<gnmi::path_elem::KeyEntry> {
    keys.into_iter()
        .map(|(key, value)| gnmi::path_elem::KeyEntry {
            key: key.to_owned(),
            value: value.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_knows_every_service_and_its_map_entries() {
        for service in [
            "gnmi.gNMI",
            "gnoi.system.System",
            "gnoi.file.File",
            "gnoi.healthz.Healthz",
            "gnoi.bgp.BGP",
            "gnoi.os.OS",
            "gnsi.authz.v1.Authz",
            "gnsi.pathz.v1.Pathz",
            "gnsi.certz.v1.Certz",
            "gnsi.credentialz.v1.Credentialz",
            "gnsi.acctz.v1.Acctz",
            "gnsi.acctz.v1.AcctzStream",
            "grpc.reflection.v1.ServerReflection",
            "grpc.reflection.v1alpha.ServerReflection",
        ] {
            assert!(pool().get_service_by_name(service).is_some(), "{service}");
        }
        assert!(is_map_entry("gnmi.PathElem.KeyEntry"));
        assert!(is_map_entry("gnoi.types.PathElem.KeyEntry"));
        assert!(!is_map_entry("gnmi.PathElem"));
    }
}
