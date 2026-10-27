import 'dart:io';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;

import 'rust/api/source.dart';
import 'rust/frb_generated.dart' show RustLib;

/// Carries the D-Bus traffic of the process-wide source.
abstract interface class Transport {
  /// Start carrying [source]'s calls. Completes once its bus answered the first ones.
  Future<void> carry(Driven source);

  /// Send the calls its source queued, and close the descriptors it released.
  void flush();
}

/// The process-wide source, as a transport sees it.
abstract interface class Driven {
  /// Its calls, replies and signals.
  MxTransport get transport;

  /// Handle outcomes, then flush every attached transport.
  void drain();
}

/// The attached transports.
final transports = <Transport>[];

/// The process-wide source, once created.
Driven? _standard;

/// Completes once every transport carrying the process-wide source has answered its first calls.
Future<void> carried = Future.value();

/// Carry the process-wide source with [transport], now or once it exists.
void attach(Transport transport) {
  transports.add(transport);
  if (_standard case final standard?) {
    carried = Future.wait([carried, transport.carry(standard)]);
  }
}

/// Stop flushing through [transport].
void detach(Transport transport) => transports.remove(transport);

/// Make [source] the process-wide source, carried by every transport.
void register(Driven source) {
  _standard = source;
  carried = Future.wait([for (final t in transports) t.carry(source)]);
}

/// The bridge initialization, shared by all callers.
Future<void>? _initialized;

/// The native library file name, matching `mxconfig-ffi`'s `[lib] name`.
const _libStem = 'libmxconfig.so';

/// Load the native library once. A failed load can be retried.
Future<void> ensureInitialized(String? libraryPath) =>
    _initialized ??= RustLib.init(
      externalLibrary: ExternalLibrary.open(_resolveLibrary(libraryPath)),
    ).catchError((Object e, StackTrace trace) {
      _initialized = null;
      Error.throwWithStackTrace(e, trace);
    });

/// Find the library: [libraryPath], then `MXCONFIG_LIB`, then the loader.
String _resolveLibrary(String? libraryPath) {
  if (libraryPath != null && libraryPath.isNotEmpty) return libraryPath;
  final env = Platform.environment['MXCONFIG_LIB'];
  if (env != null && env.isNotEmpty) return env;
  return _libStem;
}
