//! Typed, versioned TOML settings for apps.
//!
//! The crate root is the stable app API, and hidden items are internal.

#![warn(unreachable_pub)]

mod error;
#[doc(hidden)]
pub mod host;
mod lock;
mod manifest;
mod migrate;
mod paths;
mod resolve;
pub mod runtime;
mod sandbox;
mod settings;
mod source;
mod space;
mod transport;
mod watch;
mod wire;
mod write;

pub use error::{Diagnostic, Error, ErrorKind, Severity};
pub use manifest::{Access, Manifest};
pub use mxconfig_macros::client;
#[doc(hidden)]
pub use mxconfig_schema::value;
pub use mxconfig_schema::value::Value;
pub use mxconfig_schema::{AssetSchema, Class, Group, KeySchema, KeyType, Schema};
pub use paths::LayerKind;
pub use resolve::{Origin, Resolved};
pub use settings::{Asset, Edit, Settings};
pub use source::{
    Detach, Event, FromSettings, Namespace, Outcome, Source, Ticket, TicketId, Write,
};
/// A map value, used by generated clients for map keys.
pub use toml::Table;
/// A TOML datetime, used by generated clients for datetime keys.
pub use toml::value::Datetime;
pub use transport::{Bus, Change, Presence};
