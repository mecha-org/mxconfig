//! What a runtime uses to drive a [`Source`]: its file watches and its D-Bus traffic.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

#[cfg(doc)]
use crate::Source;
pub use crate::transport::{Event, Kept};
use crate::wire::{Arg, Call};
use crate::{Bus, Error};

/// The D-Bus protocol as plain values, for a transport to convert.
pub mod wire {
    pub use crate::wire::*;
}

/// Driving a [`Source`]. Nothing in a source runs on its own.
pub trait Drive {
    /// Mark `bus` as reachable by the runtime.
    fn reach(&self, bus: Bus);

    /// Mark `bus` as unreachable. Pending requests on it fail.
    fn unreach(&self, bus: Bus);

    /// Let go of `bus` while its connection lives on, detaching handles that followed its daemon.
    fn close(&self, bus: Bus);

    /// An argument for a descriptor the transport keeps open itself, by its own `id`.
    fn kept(&self, bus: Bus, id: u64) -> Arg;

    /// The ids of kept descriptors on `bus` the transport should close now.
    fn take_released(&self, bus: Bus) -> Vec<u64>;

    /// A duplicate of the inotify fd, for the runtime to read from.
    fn inotify_fd(&self) -> Result<OwnedFd, Error>;

    /// The directories to watch, if they changed since the last call.
    fn take_watch_dirs(&self) -> Option<Vec<PathBuf>>;

    /// Handle bytes read from the inotify fd.
    fn feed_inotify(&self, bytes: &[u8]);

    /// Handle a change at `path` reported by the runtime's own watcher.
    fn touched(&self, path: &Path);

    /// Re-read every local namespace.
    fn rescan(&self);

    /// Take the calls queued for `bus`, for the runtime to send.
    fn take_calls(&self, bus: Bus) -> Vec<Call>;

    /// Handle a message the runtime received from `bus`.
    fn deliver(&self, bus: Bus, event: Event);
}
