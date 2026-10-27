//! A `Source` for Dart. Dart watches files and carries D-Bus messages.

use anyhow::{Context, Result};
use flutter_rust_bridge::frb;
use mxconfig::host::Roots;
use mxconfig::runtime::Drive;
use mxconfig::runtime::Event;
use mxconfig::runtime::wire;
use mxconfig::{Presence, Schema, Source};

use super::{Change, Diagnostic, MxSettings, MxconfigException};
use crate::convert::{arg, mx_call, mx_outcome};

/// The kind of an [`MxArg`].
#[derive(Debug)]
pub enum MxArgKind {
    Str,
    U64,
    Bool,
    Fd,
    Array,
    Struct,
    Dict,
    Variant,
    /// A descriptor Dart keeps open itself, by the id in `number`.
    Kept,
    /// A value Dart could not convert.
    Unreadable,
}

/// A D-Bus value. `kind` says which fields are set.
#[derive(Debug)]
pub struct MxArg {
    pub kind: MxArgKind,
    /// A string, an array's element signature, or a sent fd's `/proc/self/fd` path.
    pub text: String,
    /// A `u64`, or a bool as 0 or 1.
    pub number: u64,
    /// A received fd's file contents, read by Dart.
    pub bytes: Vec<u8>,
    /// Array items, struct fields, dict entries as pairs, or a variant's value.
    pub items: Vec<MxArg>,
}

/// A method call for Dart to make.
#[derive(Debug)]
pub struct MxCall {
    pub id: u64,
    pub destination: String,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub args: Vec<MxArg>,
    /// Whether polkit may prompt the user.
    pub interactive: bool,
    /// Whether the reply's first argument is a descriptor to keep open, as `Kept`.
    pub keep: bool,
}

/// A signal match rule for Dart to add.
#[derive(Debug)]
pub struct MxRule {
    pub sender: String,
    pub interface: String,
    pub member: String,
    /// The required first argument, if any.
    pub arg0: Option<String>,
}

/// The kind of an [`MxOutcome`].
#[derive(Debug)]
pub enum MxOutcomeKind {
    Opened,
    Written,
    Changed,
    Asset,
    /// The namespace's handle stopped following it. `detach` says why.
    Detached,
    /// The namespace has new problems, in `diagnostics`. Log them.
    Diagnosed,
}

/// What a source knows of a bus's daemon.
#[derive(Debug)]
pub enum DaemonPresence {
    /// No transport carries the bus.
    NoBus,
    /// The bus is carried, but no daemon owns its name yet.
    NoDaemon,
    /// The daemon is running.
    Running,
}

/// Why a handle stopped following its namespace.
#[derive(Debug)]
pub enum DetachCause {
    /// Its file moved to another schema version. A restart picks that up.
    Moved,
    /// Its transport was closed. Dispose of it, and open the namespace again.
    Closed,
}

/// One outcome. `kind` says which fields are set.
#[derive(Debug)]
pub struct MxOutcome {
    pub kind: MxOutcomeKind,
    pub ticket: u64,
    pub settings: Option<MxSettings>,
    /// The value that changed, for a `Changed` outcome.
    pub change: Option<Change>,
    pub error: Option<MxconfigException>,
    /// Why the handle detached, for a `Detached` outcome.
    pub detach: Option<DetachCause>,
    /// The new problems, for a `Diagnosed` outcome.
    pub diagnostics: Vec<Diagnostic>,
    pub namespace: String,
    pub asset: Option<String>,
}

/// An mxconfig source driven by Dart.
#[frb(opaque)]
#[derive(Debug)]
pub struct MxSource {
    inner: Source,
}

impl MxSource {
    /// A source over this system's roots.
    #[frb(sync)]
    pub fn create() -> MxSource {
        MxSource {
            inner: Source::new(),
        }
    }

    /// A source over one user directory, for tests and embedding.
    #[frb(sync)]
    pub fn with_user_dir(user_dir: String) -> MxSource {
        let roots = Roots {
            user_dir: Some(user_dir.into()),
            ..Roots::default()
        };
        MxSource {
            inner: Source::with_roots(roots),
        }
    }

    /// Whether `bus` is carried and its daemon running.
    #[frb(sync)]
    pub fn daemon(&self, bus: String) -> Result<DaemonPresence, MxconfigException> {
        Ok(match self.inner.daemon(bus.parse()?) {
            Presence::NoBus => DaemonPresence::NoBus,
            Presence::Running => DaemonPresence::Running,
            // A state added later reads as no daemon.
            _ => DaemonPresence::NoDaemon,
        })
    }

    /// Carry `schema` for `namespace`: the one this client was built with.
    #[frb(sync)]
    pub fn embed(&self, namespace: String, schema: String) {
        self.inner.embed(&namespace, &schema);
    }

    /// Name the app the daemon checks manifests for.
    #[frb(sync)]
    pub fn set_app(&self, app: String) {
        self.inner.set_app(&app);
    }

    /// Start opening `namespace`, from local files or through the daemon.
    #[frb(sync)]
    pub fn request(&self, namespace: String) -> u64 {
        self.inner.request_namespace(&namespace).id().into()
    }

    /// Open `schema_toml` from local files at once.
    #[frb(sync)]
    pub fn open_schema(&self, schema_toml: String) -> Result<MxSettings, MxconfigException> {
        let schema = Schema::parse(&schema_toml).context("parsing the schema")?;
        let inner = self.inner.open_schema(schema)?;
        Ok(MxSettings { inner })
    }

    /// The directories to watch, if they changed since the last call.
    #[frb(sync)]
    pub fn take_watch_dirs(&self) -> Option<Vec<String>> {
        let dirs = self.inner.take_watch_dirs()?;
        Some(dirs.iter().map(|dir| dir.display().to_string()).collect())
    }

    /// Report a change at `path` in a watched directory.
    #[frb(sync)]
    pub fn touched(&self, path: String) {
        self.inner.touched(std::path::Path::new(&path));
    }

    /// Re-read everything, e.g. after a lost watch.
    #[frb(sync)]
    pub fn rescan(&self) {
        self.inner.rescan();
    }

    /// The transport side of this source.
    #[frb(sync)]
    pub fn transport(&self) -> MxTransport {
        MxTransport {
            inner: self.inner.clone(),
        }
    }

    /// Take the outcomes since the last call.
    #[frb(sync)]
    pub fn outcomes(&self) -> Vec<MxOutcome> {
        self.inner
            .outcomes()
            .into_iter()
            .filter_map(mx_outcome)
            .collect()
    }
}

/// What a transport drives of an [`MxSource`]: its calls, replies and signals.
#[frb(opaque)]
#[derive(Debug)]
pub struct MxTransport {
    inner: Source,
}

impl MxTransport {
    /// Mark `bus` as reachable: `session` or `system`.
    #[frb(sync)]
    pub fn reach(&self, bus: String) -> Result<(), MxconfigException> {
        self.inner.reach(bus.parse()?);
        Ok(())
    }

    /// The signals to listen to. Deliver them with `signal`.
    #[frb(sync)]
    pub fn rules() -> Vec<MxRule> {
        let rule = |rule: wire::Rule| MxRule {
            sender: rule.sender.into(),
            interface: rule.interface.into(),
            member: rule.member.into(),
            arg0: rule.arg0.map(Into::into),
        };
        wire::RULES.into_iter().map(rule).collect()
    }

    /// Take the calls queued for `bus`.
    #[frb(sync)]
    pub fn take_calls(&self, bus: String) -> Result<Vec<MxCall>, MxconfigException> {
        let calls = self.inner.take_calls(bus.parse()?);
        Ok(calls.iter().map(mx_call).collect())
    }

    /// Deliver call `id`'s reply.
    #[frb(sync)]
    pub fn reply(&self, bus: String, id: u64, args: Vec<MxArg>) -> Result<(), MxconfigException> {
        let bus = bus.parse()?;
        let args = args.into_iter().map(|a| arg(a, &self.inner, bus)).collect();
        let event = Event::Reply { id, args };
        self.inner.deliver(bus, event);
        Ok(())
    }

    /// Deliver call `id`'s D-Bus error.
    #[frb(sync)]
    pub fn fail(
        &self,
        bus: String,
        id: u64,
        name: String,
        message: String,
    ) -> Result<(), MxconfigException> {
        let event = Event::Failed { id, name, message };
        self.inner.deliver(bus.parse()?, event);
        Ok(())
    }

    /// Deliver a signal that matched one of `rules`.
    #[frb(sync)]
    pub fn signal(
        &self,
        bus: String,
        sender: String,
        interface: String,
        member: String,
        args: Vec<MxArg>,
    ) -> Result<(), MxconfigException> {
        let bus = bus.parse()?;
        let args = args.into_iter().map(|a| arg(a, &self.inner, bus)).collect();
        let event = Event::Signal {
            sender,
            interface,
            member,
            args,
        };
        self.inner.deliver(bus, event);
        Ok(())
    }

    /// Let go of `bus` while its connection lives on, detaching handles that followed its daemon.
    #[frb(sync)]
    pub fn close(&self, bus: String) -> Result<(), MxconfigException> {
        self.inner.close(bus.parse()?);
        Ok(())
    }

    /// The ids of kept descriptors on `bus` that Dart should close now.
    #[frb(sync)]
    pub fn take_released(&self, bus: String) -> Result<Vec<u64>, MxconfigException> {
        Ok(self.inner.take_released(bus.parse()?))
    }
}
