//! The schema model mxconfig and its code generator share.

#![warn(unreachable_pub)]

mod class;
mod error;
mod migration;
mod schema;
mod validate;
pub mod value;

pub use class::Class;
pub use error::{Diagnostic, Error, Severity};
pub use migration::{Migration, Op};
pub use schema::{AssetSchema, Backend, Group, KeySchema, KeyType, Retired, Schema};
pub use value::Value;
