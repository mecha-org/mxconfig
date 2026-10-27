/// For transport packages that carry mxconfig's D-Bus traffic.
library mxconfig.runtime;

export 'src/runtime.dart'
    show Driven, Transport, attach, detach, ensureInitialized;
export 'src/rust/api/source.dart'
    show MxArg, MxArgKind, MxCall, MxRule, MxTransport;
