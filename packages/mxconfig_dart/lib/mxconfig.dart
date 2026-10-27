/// Dart bindings for mxconfig.
library mxconfig;

import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'src/runtime.dart';
import 'src/rust/api.dart';
import 'src/rust/api/source.dart' as source;
import 'src/rust/api/source.dart' show DaemonPresence, DetachCause;

export 'src/rust/api.dart'
    show Change, Describe, Diagnostic, GroupInfo, KeyInfo, MxconfigException;
export 'src/rust/api/source.dart' show DaemonPresence, DetachCause;

part 'src/source.dart';

/// An open namespace.
class Settings {
  Settings._(this._inner, this._source) : namespace = _inner.namespace();

  final MxSettings _inner;
  final _Source _source;
  final String namespace;

  /// The app the daemon checks manifests for, such as `org.example.player`.
  ///
  /// Needed only by an app that reads shared settings and does not open
  /// exactly one app namespace of its own. Set it before the first open.
  static String? appId;

  /// Open a namespace with [schema], the one this app was built with.
  static Future<Settings> open(String namespace,
      {required String schema, String? libraryPath}) async {
    await ensureInitialized(libraryPath);
    final from = _Source.standard;
    if (appId case final app?) from.rust.setApp(app: app);
    from.rust.embed(namespace: namespace, schema: schema);
    return from.open(from.rust.request(namespace: namespace));
  }

  /// Open schema text over a user directory, on a separate source.
  static Future<Settings> openIn({
    required String userDir,
    required String schemaToml,
    String? libraryPath,
  }) async {
    await ensureInitialized(libraryPath);
    final from = _Source(source.MxSource.withUserDir(userDir: userDir));
    final inner = from.rust.openSchema(schemaToml: schemaToml);
    // Draining once starts watching the namespace's files.
    from.drain();
    return Settings._(inner, from);
  }

  /// Fail unless [manifestToml] grants the access.
  void authorize(String manifestToml, {bool write = false}) =>
      _inner.authorize(manifestToml: manifestToml, write: write);

  /// Raised by each batch of changes. A daemon restart starts it over.
  int get revision => _inner.revision().toInt();

  /// The value of [key], or `null` when it is unset.
  String? getString(String key) => _inner.getString(key: key);

  int? getInt(String key) => _inner.getInt(key: key)?.toInt();

  double? getDouble(String key) => _inner.getDouble(key: key);

  bool? getBool(String key) => _inner.getBool(key: key);

  /// The value as TOML text. Used for arrays and maps.
  String? getToml(String key) => _inner.getToml(key: key);

  /// Write [value] to [key]. Completes when written.
  Future<void> setString(String key, String value) =>
      _write(() => _inner.setString(key: key, value: value));

  Future<void> setInt(String key, int value) =>
      _write(() => _inner.setInt(key: key, value: value));

  Future<void> setDouble(String key, double value) =>
      _write(() => _inner.setDouble(key: key, value: value));

  Future<void> setBool(String key, bool value) =>
      _write(() => _inner.setBool(key: key, value: value));

  /// Write a TOML value such as `35` or `[1, 2]` to [key].
  Future<void> setToml(String key, String value) =>
      _write(() => _inner.setToml(key: key, value: value));

  /// Unset [key], so a lower layer or the default applies.
  Future<void> remove(String key) => _write(() => _inner.remove(key: key));

  /// Start a write, then wait for it to finish. A refusal fails the future.
  Future<void> _write(BigInt Function() start) async => _source.write(start());

  /// The bytes of asset [id], or `null` when nothing sets it.
  Uint8List? asset(String id) => _inner.asset(id: id);

  /// Write the user's copy of asset [id]. Completes when written.
  Future<void> setAsset(String id, Uint8List bytes) =>
      _write(() => _inner.setAsset(id: id, bytes: bytes));

  /// Copy the file at [path] as the user's copy of asset [id].
  Future<void> setAssetFromFile(String id, String path) =>
      _write(() => _inner.setAssetFile(id: id, path: path));

  /// Remove the user's copy of asset [id]. Completes when written.
  Future<void> removeAsset(String id) =>
      _write(() => _inner.removeAsset(id: id));

  /// Problems found while opening, migrating and reading.
  List<Diagnostic> get diagnostics => _inner.diagnostics();

  /// The schema, for settings UIs.
  Describe get describe => _inner.describe();

  /// Every value and asset change to this namespace.
  Stream<_Event> get _events =>
      _source._events.stream.where((e) => e.namespace == namespace);

  /// Every change to the namespace.
  Stream<Change> watch() => watchTyped((change) => change, (_) => null);

  /// Every change, decoded by [values] and [assets]. Others are dropped.
  Stream<T> watchTyped<T>(
          T? Function(Change) values, T? Function(String) assets) =>
      _events
          .map((e) => e.change != null ? values(e.change!) : assets(e.asset!))
          .where((typed) => typed != null)
          .cast<T>();

  /// The id of each asset whose file changed.
  Stream<String> watchAssets() => watchTyped((_) => null, (id) => id);

  /// Fires when this handle stops following its namespace, saying why.
  Stream<DetachCause> watchDetached() => _source._detached.stream
      .where((detached) => detached.$1 == namespace)
      .map((detached) => detached.$2);

  /// Whether the values come from the daemon rather than this process's own reads.
  bool get fromDaemon => _inner.fromDaemon();

  /// Whether a shared namespace still holds its schema defaults.
  bool get onDefaults => _inner.onDefaults();

  /// The version holding the namespace, when it isn't this handle's. The handle then only reads.
  int? get otherVersion => _inner.otherVersion();

  /// Whether [bus] (`session` or `system`) is carried and its daemon running.
  static Future<DaemonPresence> daemon(
      {String bus = 'session', String? libraryPath}) async {
    await ensureInitialized(libraryPath);
    final from = _Source.standard;
    // Wait for the buses to say who owns the daemon's name.
    await carried;
    return from.rust.daemon(bus: bus);
  }

  /// Release the native handle. Its subscription ends once no handle needs it.
  void dispose() {
    _inner.dispose();
    _source.drain();
  }
}
