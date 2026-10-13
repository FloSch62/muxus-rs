//! Every gNOI and gNSI RPC the UI may call, keyed by its gRPC method path.
//! Nothing outside this list reaches a device: the backend refuses unknown
//! methods instead of forwarding arbitrary calls.

use std::collections::HashMap;
use std::sync::LazyLock;

use muxus_core::management::grpc_methods;
use prost_reflect::MessageDescriptor;

use crate::proto::pool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MethodKind {
    Unary,
    ServerStream,
    ClientStream,
    Bidi,
}

impl MethodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MethodKind::Unary => "unary",
            MethodKind::ServerStream => "server-stream",
            MethodKind::ClientStream => "client-stream",
            MethodKind::Bidi => "bidi",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MethodDef {
    pub path: &'static str,
    pub kind: MethodKind,
    pub request: MessageDescriptor,
    pub response: MessageDescriptor,
}

fn define(path: &'static str) -> MethodDef {
    let (service, method) = path
        .trim_start_matches('/')
        .split_once('/')
        .expect("method paths are /service/method");
    let method = pool()
        .get_service_by_name(service)
        .and_then(|service| {
            service
                .methods()
                .find(|candidate| candidate.name() == method)
        })
        .unwrap_or_else(|| panic!("{path} is not in the vendored protos"));
    let kind = match (method.is_client_streaming(), method.is_server_streaming()) {
        (false, false) => MethodKind::Unary,
        (false, true) => MethodKind::ServerStream,
        (true, false) => MethodKind::ClientStream,
        (true, true) => MethodKind::Bidi,
    };
    MethodDef {
        path,
        kind,
        request: method.input(),
        response: method.output(),
    }
}

/// gnoi.file Get and Put have their own transfer ops with hash checks, so
/// they are not here.
static METHODS: LazyLock<HashMap<&'static str, MethodDef>> = LazyLock::new(|| {
    grpc_methods::ALL
        .iter()
        .map(|path| (*path, define(path)))
        .collect()
});

pub fn method_def(path: &str) -> Option<&'static MethodDef> {
    METHODS.get(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_every_method_the_ui_may_call_and_nothing_else() {
        for path in grpc_methods::ALL {
            assert!(method_def(path).is_some(), "{path}");
        }
        assert_eq!(
            method_def(grpc_methods::AUTHZ_ROTATE).unwrap().kind,
            MethodKind::Bidi
        );
        assert_eq!(
            method_def(grpc_methods::PING).unwrap().kind,
            MethodKind::ServerStream
        );
        assert_eq!(
            method_def(grpc_methods::TIME).unwrap().kind,
            MethodKind::Unary
        );
        assert_eq!(
            method_def(grpc_methods::ACCTZ_STREAM).unwrap().kind,
            MethodKind::ServerStream
        );
        assert_eq!(
            method_def(grpc_methods::ACCTZ_BIDI).unwrap().kind,
            MethodKind::Bidi
        );
        assert!(method_def("/gnoi.file.File/Put").is_none());
        assert!(method_def("/gnmi.gNMI/Set").is_none());
    }
}
