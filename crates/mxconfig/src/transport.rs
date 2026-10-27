//! The buses, events and descriptors a [`crate::Source`] shares with its transports.

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error::Error;
use crate::wire::Arg;
use mxconfig_schema::Class;
use mxconfig_schema::value::Value;

/// A file opened by the daemon, passed as an fd.
#[derive(Debug, Clone)]
pub(crate) struct Served {
    /// The daemon's path for it. Only for messages.
    pub(crate) path: PathBuf,
    fd: Arc<OwnedFd>,
}

impl Served {
    /// A file of this process's own, with no daemon path.
    pub(crate) fn new(fd: OwnedFd) -> Served {
        Served::from_shared(PathBuf::new(), Arc::new(fd))
    }

    pub(crate) fn from_shared(path: impl Into<PathBuf>, fd: Arc<OwnedFd>) -> Served {
        Served {
            path: path.into(),
            fd,
        }
    }

    /// The fd, shared rather than duplicated.
    pub(crate) fn shared(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.fd)
    }

    /// An anonymous file holding `bytes`.
    pub(crate) fn from_bytes(bytes: &[u8]) -> std::io::Result<Served> {
        use nix::sys::memfd::{MFdFlags, memfd_create};
        let fd = memfd_create(c"mxconfig-asset", MFdFlags::MFD_CLOEXEC)?;
        let file = std::fs::File::from(fd);
        file.write_all_at(bytes, 0)?;
        Ok(Served::new(file.into()))
    }

    /// The whole file, read from the start.
    pub(crate) fn read(&self) -> std::io::Result<Vec<u8>> {
        let file = std::fs::File::from(self.fd.try_clone()?);
        // Positional reads leave the shared file offset alone.
        let (mut bytes, mut chunk) = (Vec::new(), [0u8; 8192]);
        loop {
            match file.read_at(&mut chunk, bytes.len() as u64)? {
                0 => return Ok(bytes),
                n => bytes.extend_from_slice(&chunk[..n]),
            }
        }
    }
}

/// Equal when the path and the fd are the same.
impl PartialEq for Served {
    fn eq(&self, other: &Served) -> bool {
        self.path == other.path && self.fd.as_raw_fd() == other.fd.as_raw_fd()
    }
}

impl Eq for Served {}

/// A descriptor a transport keeps open for the source. Dropping the last clone releases it.
#[derive(Debug, Clone)]
pub struct Kept(#[expect(dead_code, reason = "held for its drop")] Arc<KeptId>);

#[derive(Debug)]
struct KeptId {
    bus: Bus,
    id: u64,
    /// Where dropped ids wait for the transport.
    released: Released,
}

/// Ids of kept descriptors the transport should close, by bus.
pub(crate) type Released = Arc<Mutex<Vec<(Bus, u64)>>>;

impl Kept {
    pub(crate) fn new(bus: Bus, id: u64, released: Released) -> Kept {
        Kept(Arc::new(KeptId { bus, id, released }))
    }
}

impl Drop for KeptId {
    fn drop(&mut self) {
        let mut released = self.released.lock().unwrap();
        released.push((self.bus, self.id));
    }
}

/// One effective value that changed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Change {
    pub namespace: String,
    pub key: String,
    /// `None` when the key became unset.
    pub value: Option<Value>,
    pub revision: u64,
}

impl Change {
    /// A change of `key` in `namespace` to `value`, `None` when unset.
    #[doc(hidden)]
    pub fn new(
        namespace: impl Into<String>,
        key: impl Into<String>,
        value: Option<Value>,
        revision: u64,
    ) -> Change {
        Change {
            namespace: namespace.into(),
            key: key.into(),
            value,
            revision,
        }
    }
}

/// A message from the bus, in the order it arrived.
#[derive(Debug)]
#[non_exhaustive]
pub enum Event {
    /// The reply to call `id`, with its arguments.
    Reply { id: u64, args: Vec<Arg> },
    /// Call `id` failed: the D-Bus error name, or empty if never sent.
    Failed {
        id: u64,
        name: String,
        message: String,
    },
    /// A signal: who sent it, its interface and member, and its arguments.
    Signal {
        sender: String,
        interface: String,
        member: String,
        args: Vec<Arg>,
    },
}

impl Event {
    /// Call `id` failed before it was sent, for the reason `message`.
    pub fn unsent(id: u64, message: impl std::fmt::Display) -> Event {
        Event::Failed {
            id,
            name: String::new(),
            message: message.to_string(),
        }
    }

    /// Call `id` got no reply in time, so it may or may not have landed.
    pub fn timed_out(id: u64) -> Event {
        Event::Failed {
            id,
            name: crate::host::protocol::NO_REPLY.into(),
            message: "no reply in time".into(),
        }
    }
}

/// The bus a request goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bus {
    /// The session daemon.
    Session,
    /// The privileged daemon, which writes privileged namespaces.
    System,
}

/// What a source knows of a bus's daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Presence {
    /// The runtime doesn't carry the bus.
    NoBus,
    /// The bus is carried, but no daemon owns its name yet.
    NoDaemon,
    /// The daemon is running.
    Running,
}

impl Bus {
    /// The bus whose daemon serves namespaces of `class`.
    pub fn serving(class: Class) -> Bus {
        match class {
            Class::Privileged => Bus::System,
            Class::App | Class::Desktop => Bus::Session,
        }
    }
}

/// Parses `session` or `system`.
impl std::str::FromStr for Bus {
    type Err = Error;

    fn from_str(name: &str) -> Result<Bus, Error> {
        match name {
            "session" => Ok(Bus::Session),
            "system" => Ok(Bus::System),
            _ => Err(Error::invalid(
                "bus",
                format!("is `{name}`, not `session` or `system`"),
            )),
        }
    }
}

impl std::fmt::Display for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Bus::Session => "mxconfigd",
            Bus::System => "the privileged service",
        })
    }
}
