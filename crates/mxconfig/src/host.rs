//! What mxconfig's own tools use: roots, sandboxes, manifests and the D-Bus protocol. Not a stable API.

#[path = "protocol.rs"]
pub mod protocol;

pub use crate::paths::{ADMIN_BASE, Home, Roots};
pub use crate::sandbox::Sandbox;
