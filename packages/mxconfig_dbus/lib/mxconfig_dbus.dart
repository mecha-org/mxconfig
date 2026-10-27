/// Carries mxconfig's D-Bus traffic over the app's own `package:dbus` client.
library mxconfig_dbus;

import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:dbus/dbus.dart';
import 'package:mxconfig_dart/runtime.dart';

/// How long a call may wait for its reply, as libdbus allows.
const _timeout = Duration(seconds: 25);

/// How long a call that may ask the user to authenticate may wait.
const _interactiveTimeout = Duration(minutes: 5);

/// The D-Bus error for a call that got no reply.
const _noReply = 'org.freedesktop.DBus.Error.NoReply';

/// Carries the library's D-Bus calls and signals.
class DBusTransport implements Transport {
  DBusTransport._(this._client, this._bus);

  final DBusClient _client;
  final String _bus;

  /// The process-wide source, once it is carried.
  Driven? _from;
  final _listening = <StreamSubscription<Object?>>[];

  /// Subscription descriptors kept for the library, by id. Closing one ends it.
  final _kept = <int, RandomAccessFile>{};
  var _nextKept = 0;

  /// Send calls for [bus] (`session` or `system`) over [client].
  static Future<DBusTransport> install(DBusClient client,
      {String bus = 'session', String? libraryPath}) async {
    await ensureInitialized(libraryPath);
    final transport = DBusTransport._(client, bus);
    transport._listen();
    attach(transport);
    return transport;
  }

  /// Stop carrying the bus, detaching handles that followed its daemon. The client stays open.
  Future<void> close() async {
    detach(this);
    if (_from case final from?) {
      _from = null;
      from.transport.close(bus: _bus);
      _closeReleased(from);
      from.drain();
    }
    await Future.wait([for (final s in _listening) s.cancel()]);
  }

  /// Close the kept descriptors [from] no longer needs, ending their subscriptions.
  void _closeReleased(Driven from) {
    for (final id in from.transport.takeReleased(bus: _bus)) {
      _kept.remove(id.toInt())?.closeSync();
    }
  }

  @override
  Future<void> carry(Driven from) async {
    _from = from;
    from.transport.reach(bus: _bus);
    // Ask who owns the daemon's name now, not at the next open.
    await Future.wait(_send(from));
  }

  @override
  void flush() {
    final from = _from;
    if (from == null) return;
    _closeReleased(from);
    unawaited(Future.wait(_send(from)));
  }

  /// Send the calls [from] queued for this bus. Each completes once answered.
  List<Future<void>> _send(Driven from) => [
        for (final call in from.transport.takeCalls(bus: _bus))
          _call(from, call)
      ];

  void _listen() {
    for (final rule in MxTransport.rules()) {
      final arg0 =
          switch (rule.arg0) { final arg0? => DBusString(arg0), _ => null };
      // package:dbus match rules lack arg0.
      final signals = DBusSignalStream(_client,
              sender: rule.sender,
              interface: rule.interface_,
              name: rule.member)
          .where((signal) => arg0 == null || signal.values.firstOrNull == arg0);
      _listening.add(signals.listen((signal) {
        final from = _from;
        if (from == null) return;
        from.transport.signal(
            bus: _bus,
            sender: signal.sender ?? '',
            interface_: signal.interface,
            member: signal.name,
            args: signal.values.map(_arg).toList());
        from.drain();
      }));
    }
  }

  /// Make one call and give the reply or error to [from].
  Future<void> _call(Driven from, MxCall call) async {
    // Opened for the call only. The library keeps the fd itself open.
    final opened = <RandomAccessFile>[];
    try {
      final reply = await _client
          .callMethod(
              destination: call.destination,
              path: DBusObjectPath(call.path),
              interface: call.interface_,
              name: call.member,
              values: call.args.map((arg) => _value(arg, opened)).toList(),
              allowInteractiveAuthorization: call.interactive)
          // Without an answer in time, a write may or may not have landed.
          .timeout(call.interactive ? _interactiveTimeout : _timeout);
      final values = [
        for (final (i, value) in reply.values.indexed)
          i == 0 && call.keep ? _keep(from, value) : _arg(value)
      ];
      from.transport.reply(bus: _bus, id: call.id, args: values);
    } on DBusMethodResponseException catch (e) {
      final values = e.response.values;
      final message =
          values.isEmpty ? e.errorName : values.first.toNative().toString();
      from.transport
          .fail(bus: _bus, id: call.id, name: e.errorName, message: message);
    } on TimeoutException {
      from.transport
          .fail(bus: _bus, id: call.id, name: _noReply, message: 'no reply');
    } catch (e) {
      // No error name means the call was never sent.
      from.transport.fail(bus: _bus, id: call.id, name: '', message: '$e');
    } finally {
      for (final file in opened) {
        file.closeSync();
      }
    }
    from.drain();
  }

  /// A subscription descriptor kept here, as the library can't take one from Dart.
  MxArg _keep(Driven from, DBusValue value) {
    if (value is! DBusUnixFd) return _arg(value);
    final kept = value.handle.toFile();
    // Closed meanwhile, so the subscription ends at once.
    if (!identical(_from, from)) {
      kept.closeSync();
      return _mxArg(MxArgKind.unreadable);
    }
    final id = _nextKept++;
    _kept[id] = kept;
    return _mxArg(MxArgKind.kept, number: id);
  }

  /// [arg] as a D-Bus value. A sent fd's file is added to [opened].
  static DBusValue _value(MxArg arg, List<RandomAccessFile> opened) {
    final items = arg.items.map((item) => _value(item, opened));
    return switch (arg.kind) {
      MxArgKind.str => DBusString(arg.text),
      MxArgKind.bool => DBusBoolean(arg.number != BigInt.zero),
      MxArgKind.fd => DBusUnixFd(_open(arg.text, opened)),
      MxArgKind.array => DBusArray(DBusSignature(arg.text), items),
      MxArgKind.struct => DBusStruct(items),
      _ => throw StateError('mxconfig never sends ${arg.kind}'),
    };
  }

  /// A handle on the file at [path], added to [opened].
  static ResourceHandle _open(String path, List<RandomAccessFile> opened) {
    final file = File(path).openSync();
    opened.add(file);
    return ResourceHandle.fromFile(file);
  }

  /// An argument of [kind], with only the fields it uses set.
  static MxArg _mxArg(MxArgKind kind,
          {String text = '',
          int number = 0,
          Uint8List? bytes,
          Iterable<DBusValue> items = const []}) =>
      MxArg(
          kind: kind,
          text: text,
          number: BigInt.from(number).toUnsigned(64),
          bytes: bytes ?? Uint8List(0),
          items: items.map(_arg).toList());

  /// A D-Bus value for the library. A received fd is read now, in order.
  static MxArg _arg(DBusValue value) => switch (value) {
        DBusString(:final value) => _mxArg(MxArgKind.str, text: value),
        DBusUint64(:final value) => _mxArg(MxArgKind.u64, number: value),
        DBusBoolean(:final value) =>
          _mxArg(MxArgKind.bool, number: value ? 1 : 0),
        DBusUnixFd(:final handle) => switch (_read(handle)) {
            final bytes? => _mxArg(MxArgKind.fd, bytes: bytes),
            null => _mxArg(MxArgKind.unreadable),
          },
        DBusArray(:final childSignature, :final children) =>
          _mxArg(MxArgKind.array, text: childSignature.value, items: children),
        DBusStruct(:final children) =>
          _mxArg(MxArgKind.struct, items: children),
        DBusDict(:final children) => _mxArg(MxArgKind.dict,
            items: children.entries.map((e) => DBusStruct([e.key, e.value]))),
        DBusVariant(:final value) => _mxArg(MxArgKind.variant, items: [value]),
        _ => _mxArg(MxArgKind.unreadable),
      };

  /// The contents of a received fd's file, or null if unreadable.
  static Uint8List? _read(ResourceHandle handle) {
    RandomAccessFile? file;
    try {
      file = handle.toFile();
      return file.readSync(file.lengthSync());
    } catch (_) {
      return null;
    } finally {
      file?.closeSync();
    }
  }
}
