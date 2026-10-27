//! mxconfigd: serves installed namespaces, and those apps register, over D-Bus.

#![warn(unreachable_pub)]

mod policy;
mod service;

pub use service::Service;
