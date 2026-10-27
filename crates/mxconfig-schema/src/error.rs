use std::fmt;

/// A schema, namespace id or value that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(transparent)]
pub struct Error(#[from] Kind);

/// What could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Kind {
    #[error("invalid schema: {0}")]
    Schema(String),

    #[error("invalid namespace id `{0}`")]
    Namespace(String),

    #[error("invalid TOML value: {0}")]
    Value(String),
}

impl Error {
    /// Whether this is a value that is not TOML.
    pub fn is_value(&self) -> bool {
        matches!(self.0, Kind::Value(_))
    }
}

/// How bad a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        })
    }
}

/// A problem with a key or file that does not stop resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    severity: Severity,
    key: Option<String>,
    source: Option<String>,
    message: String,
}

/// Formats as one log line: the severity, the file, then the message.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.severity)?;
        if let Some(source) = &self.source {
            write!(f, "{source}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl Diagnostic {
    #[doc(hidden)]
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Severity::Error, message)
    }

    #[doc(hidden)]
    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, message)
    }

    pub fn severity(&self) -> Severity {
        self.severity
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// The key it is about, if any.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The file or env var it came from, if any.
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            key: None,
            source: None,
            message: message.into(),
        }
    }
}
