//! Connects a [`Source`] to mxconfigd over a zbus connection.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use mxconfig::host::protocol::{ADD_MATCH, DBUS, DBUS_PATH};
use mxconfig::runtime::Drive;
use mxconfig::runtime::Event;
use mxconfig::runtime::wire::{Arg, Call, RULES, Rule};
use mxconfig::{Bus, Source};
use tokio::time::Instant;
use zbus::message::{Flags, Type};
use zbus::zvariant::{Array, Fd, Signature, Structure, StructureBuilder, Value};
use zbus::{Message, MessageStream};

/// How long a call may wait for its reply, as libdbus allows.
const TIMEOUT: Duration = Duration::from_secs(25);

/// How long a call that may ask the user to authenticate may wait.
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(300);

/// Carries a source's traffic on one bus.
#[derive(Debug)]
pub struct ZbusLink {
    conn: zbus::Connection,
    bus: Bus,
    incoming: MessageStream,
    /// Request ids of the calls in flight and when each gives up, by serial.
    waiting: BTreeMap<NonZeroU32, (u64, Instant)>,
    source: Source,
}

impl ZbusLink {
    /// Connect `source`'s requests for `bus` to `conn`.
    pub async fn new(conn: &zbus::Connection, bus: Bus, source: Source) -> zbus::Result<Self> {
        let incoming = MessageStream::from(conn);
        for rule in RULES {
            let rule = (rule.text(),);
            conn.call_method(Some(DBUS), DBUS_PATH, Some(DBUS), ADD_MATCH, &rule)
                .await?;
        }
        source.reach(bus);
        Ok(Self {
            conn: conn.clone(),
            bus,
            incoming,
            waiting: BTreeMap::new(),
            source,
        })
    }

    /// Stop carrying the bus, detaching handles that followed its daemon.
    pub fn close(self) {
        self.source.close(self.bus);
    }

    /// Send queued requests and handle messages until all calls are answered.
    pub async fn settle(&mut self) {
        loop {
            self.send_queued().await;
            if self.waiting.is_empty() || !self.receive().await {
                return;
            }
        }
    }

    /// Send queued requests, then handle one message. `false` once closed.
    pub async fn next(&mut self) -> bool {
        self.send_queued().await;
        self.receive().await
    }

    /// Handle one message, or calls that waited too long. `false` once closed.
    async fn receive(&mut self) -> bool {
        let first = self.waiting.values().map(|&(_, at)| at).min();
        let next = self.incoming.next();
        let message = match first {
            Some(at) => tokio::select! {
                message = next => message,
                () = tokio::time::sleep_until(at) => {
                    self.expire();
                    return true;
                }
            },
            None => next.await,
        };
        let Some(message) = message else {
            return false;
        };
        if let Ok(message) = message
            && let Some(event) = self.event(&message)
        {
            self.source.deliver(self.bus, event);
        }
        true
    }

    /// Fail the calls whose time ran out. A write's may still have landed.
    fn expire(&mut self) {
        let now = Instant::now();
        let late = self.waiting.extract_if(.., |_, &mut (_, at)| at <= now);
        let late: Vec<u64> = late.map(|(_, (id, _))| id).collect();
        for id in late {
            self.source.deliver(self.bus, Event::timed_out(id));
        }
    }

    async fn send_queued(&mut self) {
        for call in self.source.take_calls(self.bus) {
            match self.send(&call).await {
                Ok(message) => {
                    let serial = message.primary_header().serial_num();
                    let limit = if call.interactive {
                        INTERACTIVE_TIMEOUT
                    } else {
                        TIMEOUT
                    };
                    self.waiting
                        .insert(serial, (call.id, Instant::now() + limit));
                }
                Err(e) => self.source.deliver(self.bus, Event::unsent(call.id, e)),
            }
        }
    }

    /// Send `call`. Returns the message, for its serial.
    async fn send(&self, call: &Call) -> zbus::Result<Message> {
        let mut builder = Message::method_call(call.path, call.member)?
            .destination(call.destination)?
            .interface(call.interface)?;
        if call.interactive {
            builder = builder.with_flags(Flags::AllowInteractiveAuth)?;
        }
        let message = builder.build(&structure(&call.args)?)?;
        self.conn.send(&message).await?;
        Ok(message)
    }

    /// The event for `message`, if the source should see it.
    fn event(&mut self, message: &Message) -> Option<Event> {
        let header = message.header();
        match message.message_type() {
            Type::MethodReturn => {
                let (id, _) = self.waiting.remove(&header.reply_serial()?)?;
                let args = args(message);
                Some(Event::Reply { id, args })
            }
            Type::Error => {
                let (id, _) = self.waiting.remove(&header.reply_serial()?)?;
                let name = header.error_name()?.to_string();
                let message = message.body().deserialize().unwrap_or_default();
                Some(Event::Failed { id, name, message })
            }
            Type::Signal => {
                let (interface, member) = (header.interface()?, header.member()?);
                // Skip the connection's other signals before reading their bodies.
                let ours = |rule: &Rule| {
                    rule.interface == interface.as_str() && rule.member == member.as_str()
                };
                if !RULES.iter().any(ours) {
                    return None;
                }
                Some(Event::Signal {
                    sender: header.sender()?.to_string(),
                    interface: interface.to_string(),
                    member: member.to_string(),
                    args: args(message),
                })
            }
            Type::MethodCall => None,
        }
    }
}

/// Dropping only fails what waits on the bus. [`ZbusLink::close`] also detaches handles.
impl Drop for ZbusLink {
    fn drop(&mut self) {
        self.source.unreach(self.bus);
    }
}

/// A message body's arguments, or none if it can't be read.
fn args(message: &Message) -> Vec<Arg> {
    let body = message.body();
    if *body.signature() == Signature::Unit {
        return Vec::new();
    }
    body.deserialize::<Structure>()
        .map(|body| body.fields().iter().map(arg).collect())
        .unwrap_or_default()
}

/// Arguments as a zvariant structure.
fn structure(args: &[Arg]) -> zbus::zvariant::Result<Structure<'_>> {
    args.iter()
        .map(value)
        .fold(StructureBuilder::new(), StructureBuilder::append_field)
        .build()
}

/// `arg` as a zvariant value, for the kinds our calls send.
fn value(arg: &Arg) -> Value<'_> {
    match arg {
        Arg::Str(text) => Value::from(text.as_str()),
        Arg::Bool(b) => Value::Bool(*b),
        Arg::Fd(fd) => Value::Fd(Fd::from(&**fd)),
        Arg::Array(signature, items) => {
            let signature = Signature::try_from(signature.as_str()).expect("a valid signature");
            let mut array = Array::new(&signature);
            for item in items {
                array
                    .append(value(item))
                    .expect("items match the signature");
            }
            Value::Array(array)
        }
        Arg::Struct(fields) => Value::Structure(structure(fields).expect("a struct has fields")),
        Arg::U64(_) | Arg::Variant(_) | Arg::Dict(_) | Arg::Kept(_) | Arg::Unreadable => {
            unreachable!("mxconfig sends none of these")
        }
    }
}

/// A received zvariant value as an [`Arg`]. Other kinds are unreadable.
fn arg(value: &Value<'_>) -> Arg {
    match value {
        Value::Str(text) => Arg::Str(text.to_string()),
        Value::U64(n) => Arg::U64(*n),
        Value::Bool(b) => Arg::Bool(*b),
        Value::Fd(fd) => fd
            .as_fd()
            .try_clone_to_owned()
            .map_or(Arg::Unreadable, |fd| Arg::Fd(Arc::new(fd))),
        Value::Array(array) => {
            let signature = array.element_signature().to_string();
            Arg::Array(signature, array.inner().iter().map(arg).collect())
        }
        Value::Structure(fields) => Arg::Struct(fields.fields().iter().map(arg).collect()),
        Value::Dict(dict) => Arg::Dict(dict.iter().map(|(k, v)| (arg(k), arg(v))).collect()),
        Value::Value(inner) => Arg::Variant(Box::new(arg(inner))),
        _ => Arg::Unreadable,
    }
}
