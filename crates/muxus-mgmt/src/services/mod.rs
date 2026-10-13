//! gNOI and gNSI: which calls the UI may make, the JSON form of their
//! messages, and the file transfers and reflection the backend runs itself.

pub mod json;
pub mod registry;
pub mod transfer;

pub use registry::{MethodDef, MethodKind, method_def};
