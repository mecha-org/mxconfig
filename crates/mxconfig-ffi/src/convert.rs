//! Conversions between mxconfig's types and the ones bridged to Dart.

use std::os::fd::AsRawFd;

use mxconfig::runtime::Drive;
use mxconfig::runtime::wire::{Arg, Call};
use mxconfig::{Bus, Detach, Outcome, Source, State, Unserved};

use crate::api::source::{DetachCause, MxArg, MxArgKind, MxCall, MxOutcome, MxOutcomeKind};
use crate::api::{Change, Diagnostic, MxSettings, MxState, MxStateKind, MxUnserved};

/// `d` for Dart.
pub(crate) fn mx_diagnostic(d: &mxconfig::Diagnostic) -> Diagnostic {
    Diagnostic {
        severity: d.severity().to_string(),
        key: d.key().map(str::to_string),
        source: d.source().map(str::to_string),
        message: d.message().to_string(),
    }
}

pub(crate) fn mx_outcome(outcome: Outcome) -> Option<MxOutcome> {
    let blank = |kind| MxOutcome {
        kind,
        ticket: 0,
        settings: None,
        change: None,
        error: None,
        detach: None,
        diagnostics: Vec::new(),
        current: Vec::new(),
        namespace: String::new(),
        asset: None,
    };
    let outcome = match outcome {
        Outcome::Opened {
            ticket,
            namespace,
            settings,
        } => MxOutcome {
            ticket: ticket.into(),
            settings: Some(MxSettings { inner: settings }),
            namespace,
            ..blank(MxOutcomeKind::Opened)
        },
        // Dart tells a failure by its error.
        Outcome::OpenFailed {
            ticket,
            namespace,
            error,
        } => MxOutcome {
            ticket: ticket.into(),
            error: Some(error.into()),
            namespace,
            ..blank(MxOutcomeKind::Opened)
        },
        Outcome::WriteFailed {
            namespace,
            error,
            current,
            asset,
            ..
        } => MxOutcome {
            error: Some(error.into()),
            current: current.into_iter().map(mx_change).collect(),
            namespace,
            asset,
            ..blank(MxOutcomeKind::WriteFailed)
        },
        Outcome::Changed(change) => MxOutcome {
            namespace: change.namespace.clone(),
            change: Some(mx_change(change)),
            ..blank(MxOutcomeKind::Changed)
        },
        Outcome::AssetChanged { namespace, id } => MxOutcome {
            namespace,
            asset: Some(id),
            ..blank(MxOutcomeKind::Asset)
        },
        Outcome::Detached { namespace, cause } => MxOutcome {
            namespace,
            detach: Some(detach_cause(cause)),
            ..blank(MxOutcomeKind::Detached)
        },
        Outcome::Diagnosed {
            namespace,
            diagnostics,
        } => MxOutcome {
            namespace,
            diagnostics: diagnostics.iter().map(mx_diagnostic).collect(),
            ..blank(MxOutcomeKind::Diagnosed)
        },
        // Dart has no kind for an outcome added later.
        _ => return None,
    };
    Some(outcome)
}

fn detach_cause(cause: Detach) -> DetachCause {
    match cause {
        Detach::Moved => DetachCause::Moved,
        // A cause added later reads as a transport close: open it again.
        _ => DetachCause::Closed,
    }
}

/// `state` for Dart.
pub(crate) fn mx_state(state: State) -> MxState {
    let blank = |kind| MxState {
        kind,
        version: None,
        unserved: None,
        detach: None,
    };
    match state {
        State::ReadOnly { holder } => MxState {
            version: Some(holder),
            ..blank(MxStateKind::ReadOnly)
        },
        State::Defaults(why) => {
            let (unserved, version) = match why {
                Unserved::NoBus => (MxUnserved::NoBus, None),
                Unserved::NoDaemon => (MxUnserved::NoDaemon, None),
                Unserved::Version { served } => (MxUnserved::Version, served),
                // A reason added later reads as an unserved namespace.
                _ => (MxUnserved::NotServed, None),
            };
            MxState {
                version,
                unserved: Some(unserved),
                ..blank(MxStateKind::Defaults)
            }
        }
        State::Detached(cause) => MxState {
            detach: Some(detach_cause(cause)),
            ..blank(MxStateKind::Detached)
        },
        // A state added later reads as live.
        _ => blank(MxStateKind::Live),
    }
}

pub(crate) fn mx_call(call: &Call) -> MxCall {
    MxCall {
        id: call.id,
        destination: call.destination.into(),
        path: call.path.into(),
        interface: call.interface.into(),
        member: call.member.into(),
        args: call.args.iter().map(mx_arg).collect(),
        interactive: call.interactive,
        keep: call.keep,
    }
}

/// `arg` for Dart. A sent fd stays open while its call is pending.
fn mx_arg(arg: &Arg) -> MxArg {
    let blank = |kind| MxArg {
        kind,
        text: String::new(),
        number: 0,
        bytes: Vec::new(),
        items: Vec::new(),
    };
    let items = |kind, items: &[Arg]| MxArg {
        items: items.iter().map(mx_arg).collect(),
        ..blank(kind)
    };
    match arg {
        Arg::Str(text) => MxArg {
            text: text.clone(),
            ..blank(MxArgKind::Str)
        },
        Arg::Bool(b) => MxArg {
            number: u64::from(*b),
            ..blank(MxArgKind::Bool)
        },
        Arg::Fd(fd) => MxArg {
            text: format!("/proc/self/fd/{}", fd.as_raw_fd()),
            ..blank(MxArgKind::Fd)
        },
        Arg::Array(signature, all) => MxArg {
            text: signature.clone(),
            ..items(MxArgKind::Array, all)
        },
        Arg::Struct(fields) => items(MxArgKind::Struct, fields),
        // mxconfig sends none of these.
        Arg::U64(_) | Arg::Dict(_) | Arg::Variant(_) | Arg::Kept(_) | Arg::Unreadable => {
            blank(MxArgKind::Unreadable)
        }
    }
}

/// A value from Dart on `bus`. A received fd's bytes go into an anonymous file.
pub(crate) fn arg(arg: MxArg, source: &Source, bus: Bus) -> Arg {
    let MxArg {
        kind,
        text,
        number,
        bytes,
        items,
    } = arg;
    let mut items = items.into_iter().map(|item| self::arg(item, source, bus));
    match kind {
        MxArgKind::Str => Arg::Str(text),
        MxArgKind::U64 => Arg::U64(number),
        MxArgKind::Bool => Arg::Bool(number != 0),
        MxArgKind::Fd => Arg::file(&bytes).unwrap_or(Arg::Unreadable),
        MxArgKind::Array => Arg::Array(text, items.collect()),
        MxArgKind::Struct => Arg::Struct(items.collect()),
        MxArgKind::Dict => {
            let pair = |entry: Arg| match entry {
                Arg::Struct(pair) => <[Arg; 2]>::try_from(pair).ok().map(|[k, v]| (k, v)),
                _ => None,
            };
            let pairs: Option<Vec<_>> = items.map(pair).collect();
            pairs.map_or(Arg::Unreadable, Arg::Dict)
        }
        MxArgKind::Variant => items
            .next()
            .map_or(Arg::Unreadable, |inner| Arg::Variant(Box::new(inner))),
        MxArgKind::Kept => source.kept(bus, number),
        MxArgKind::Unreadable => Arg::Unreadable,
    }
}

fn mx_change(change: mxconfig::Change) -> Change {
    let value = change.value.as_ref();
    Change {
        string_value: value.and_then(mxconfig::value::to_text),
        int_value: value.and_then(mxconfig::Value::as_integer),
        double_value: value.and_then(mxconfig::value::as_f64),
        bool_value: value.and_then(mxconfig::Value::as_bool),
        value: value.map(ToString::to_string),
        namespace: change.namespace,
        key: change.key,
    }
}
