part of '../mxconfig.dart';

/// A change to a namespace: a value, or else an asset's id.
typedef _Event = ({String namespace, Change? change, String? asset});

/// A Rust source and the operations Dart is waiting on.
class _Source implements Driven {
  /// A source no transport carries.
  _Source(this.rust);

  /// The process-wide source, carried by every transport.
  _Source._standard() : rust = source.MxSource.create() {
    register(this);
  }

  final source.MxSource rust;

  /// Opens waiting on their outcome, by ticket.
  final _pending = <int, Completer<Settings>>{};
  final _events = StreamController<_Event>.broadcast(sync: true);
  final _detached =
      StreamController<(String, DetachCause)>.broadcast(sync: true);
  final _failed =
      StreamController<(String, WriteFailure)>.broadcast(sync: true);
  final _watching = <String, StreamSubscription<FileSystemEvent>>{};

  /// The directories the source last asked to watch.
  var _wanted = <String>{};

  /// The process-wide source, over this system's roots.
  static final standard = _Source._standard();

  @override
  late final transport = rust.transport();

  /// Wait for the open of [ticket].
  Future<Settings> open(BigInt ticket) {
    final done = _pending[ticket.toInt()] = Completer<Settings>();
    drain();
    return done.future;
  }

  /// Handle outcomes, then send requests and update watches.
  @override
  void drain() {
    for (final outcome in rust.outcomes()) {
      final (ticket, namespace) = (outcome.ticket.toInt(), outcome.namespace);
      final error = outcome.error;
      switch (outcome.kind) {
        case source.MxOutcomeKind.opened when error != null:
          _log('error: cannot open `$namespace`: ${error.message}');
          _pending.remove(ticket)?.completeError(error);
        case source.MxOutcomeKind.opened:
          _pending
              .remove(ticket)
              ?.complete(Settings._(outcome.settings!, this));
        case source.MxOutcomeKind.writeFailed:
          final failure =
              WriteFailure._(error!, outcome.current, outcome.asset);
          _failed.add((namespace, failure));
        case source.MxOutcomeKind.changed:
          _events
              .add((namespace: namespace, change: outcome.change, asset: null));
        case source.MxOutcomeKind.asset:
          _events
              .add((namespace: namespace, change: null, asset: outcome.asset));
        case source.MxOutcomeKind.detached:
          _detached.add((namespace, outcome.detach!));
        case source.MxOutcomeKind.diagnosed:
          for (final d in outcome.diagnostics) {
            final from = d.source == null ? '' : '${d.source}: ';
            _log('${d.severity}: $from${d.message}');
          }
      }
    }
    _watch();
    for (final transport in transports) {
      transport.flush();
    }
  }

  /// Watch the directories the source needs, and any lost watch again.
  void _watch() {
    final dirs = rust.takeWatchDirs();
    if (dirs != null) {
      _wanted = dirs.toSet();
      _watching.removeWhere((dir, watch) {
        final gone = !_wanted.contains(dir);
        if (gone) watch.cancel();
        return gone;
      });
    }
    for (final dir in _wanted) {
      _watching.putIfAbsent(
          dir,
          () => Directory(dir).watch().listen((event) {
                rust.touched(path: event.path);
                drain();
              }, onError: (_) => _lost(dir), onDone: () => _lost(dir)));
    }
  }

  /// Log [line] for whoever runs the app.
  static void _log(String line) => stderr.writeln('mxconfig: $line');

  /// A watch was lost: re-read everything and watch again.
  void _lost(String dir) {
    if (_watching.remove(dir) == null) return;
    rust.rescan();
    drain();
  }
}
