//! Error taxonomy and the stable exit-code table [M1.2].
//!
//! Every failure path in jired maps to exactly one [`ExitCode`]. The mapping is a
//! public contract documented in `docs/plan/commands.md` and consumed by agents,
//! so it must never drift silently — see `tests/exit_codes.rs`.

use std::fmt;

/// Stable process exit codes (decision D15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    Generic = 1,
    Usage = 2,
    Auth = 3,
    Network = 4,
    Conflict = 5,
    LossyBlocked = 6,
    MissingValue = 7,
    Locked = 8,
    CorruptState = 9,
    UnsupportedServer = 10,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Machine-readable error category, emitted as `kind` in `jired/error/1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Generic,
    Usage,
    Auth,
    Network,
    Conflict,
    Lossy,
    MissingValue,
    Locked,
    CorruptState,
    UnsupportedServer,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Generic => "generic",
            ErrorKind::Usage => "usage",
            ErrorKind::Auth => "auth",
            ErrorKind::Network => "network",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Lossy => "lossy",
            ErrorKind::MissingValue => "missing_value",
            ErrorKind::Locked => "locked",
            ErrorKind::CorruptState => "corrupt_state",
            ErrorKind::UnsupportedServer => "unsupported_server",
        }
    }

    pub fn exit_code(self) -> ExitCode {
        match self {
            ErrorKind::Generic => ExitCode::Generic,
            ErrorKind::Usage => ExitCode::Usage,
            ErrorKind::Auth => ExitCode::Auth,
            ErrorKind::Network => ExitCode::Network,
            ErrorKind::Conflict => ExitCode::Conflict,
            ErrorKind::Lossy => ExitCode::LossyBlocked,
            ErrorKind::MissingValue => ExitCode::MissingValue,
            ErrorKind::Locked => ExitCode::Locked,
            ErrorKind::CorruptState => ExitCode::CorruptState,
            ErrorKind::UnsupportedServer => ExitCode::UnsupportedServer,
        }
    }
}

/// The single error type crossing command boundaries.
///
/// The payload is boxed so `Result<T>` stays pointer-sized: almost every
/// function in jired returns one, and the error path is the rare one.
/// Field access still reads naturally through [`Deref`](std::ops::Deref).
#[derive(Debug)]
pub struct JiredError(Box<ErrorRepr>);

/// Contents of a [`JiredError`].
///
/// `details` carries structured context that is serialized verbatim into the
/// `details` object of `jired/error/1`; it must never contain secrets.
#[derive(Debug)]
pub struct ErrorRepr {
    pub kind: ErrorKind,
    pub message: String,
    pub details: serde_json::Value,
    pub hint: Option<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl std::ops::Deref for JiredError {
    type Target = ErrorRepr;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl JiredError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self(Box::new(ErrorRepr {
            kind,
            message: message.into(),
            details: serde_json::Value::Object(Default::default()),
            hint: None,
            source: None,
        }))
    }

    pub fn generic(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Generic, message)
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }
    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Auth, message)
    }
    pub fn network(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Network, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }
    pub fn lossy(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Lossy, message)
    }
    pub fn missing_value(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::MissingValue, message)
    }
    pub fn locked(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Locked, message)
    }
    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::CorruptState, message)
    }
    pub fn unsupported_server(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedServer, message)
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.0.details = details;
        self
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        if let serde_json::Value::Object(map) = &mut self.0.details {
            map.insert(key.to_string(), value.into());
        }
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.0.hint = Some(hint.into());
        self
    }

    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.0.source = Some(Box::new(source));
        self
    }

    pub fn exit_code(&self) -> ExitCode {
        self.0.kind.exit_code()
    }

    /// The `jired/error/1` payload written to stderr in `--json` mode.
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("schema".into(), "jired/error/1".into());
        obj.insert(
            "code".into(),
            serde_json::Value::from(self.exit_code().as_i32()),
        );
        obj.insert("kind".into(), self.0.kind.as_str().into());
        obj.insert("message".into(), self.0.message.clone().into());
        obj.insert("details".into(), self.0.details.clone());
        if let Some(hint) = &self.0.hint {
            obj.insert("hint".into(), hint.clone().into());
        }
        serde_json::Value::Object(obj)
    }
}

impl fmt::Display for JiredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.message)?;
        if let Some(source) = &self.0.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for JiredError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0
            .source
            .as_ref()
            .map(|b| b.as_ref() as &(dyn std::error::Error + 'static))
    }
}

pub type Result<T> = std::result::Result<T, JiredError>;

impl From<std::io::Error> for JiredError {
    fn from(e: std::io::Error) -> Self {
        JiredError::generic(format!("i/o error: {e}")).with_source(e)
    }
}

impl From<rusqlite::Error> for JiredError {
    fn from(e: rusqlite::Error) -> Self {
        JiredError::corrupt(format!("database error: {e}")).with_source(e)
    }
}

impl From<serde_json::Error> for JiredError {
    fn from(e: serde_json::Error) -> Self {
        JiredError::generic(format!("json error: {e}")).with_source(e)
    }
}

impl From<serde_yaml_ng::Error> for JiredError {
    fn from(e: serde_yaml_ng::Error) -> Self {
        JiredError::generic(format!("yaml error: {e}")).with_source(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exit-code table is a public contract; this test is its lock.
    #[test]
    fn kind_to_exit_code_mapping_is_stable() {
        let cases = [
            (ErrorKind::Generic, 1, "generic"),
            (ErrorKind::Usage, 2, "usage"),
            (ErrorKind::Auth, 3, "auth"),
            (ErrorKind::Network, 4, "network"),
            (ErrorKind::Conflict, 5, "conflict"),
            (ErrorKind::Lossy, 6, "lossy"),
            (ErrorKind::MissingValue, 7, "missing_value"),
            (ErrorKind::Locked, 8, "locked"),
            (ErrorKind::CorruptState, 9, "corrupt_state"),
            (ErrorKind::UnsupportedServer, 10, "unsupported_server"),
        ];
        for (kind, code, name) in cases {
            assert_eq!(kind.exit_code().as_i32(), code, "{name}");
            assert_eq!(kind.as_str(), name);
        }
    }

    /// `Result<T>` is on every hot path; the error must not bloat it.
    #[test]
    fn result_stays_small() {
        assert_eq!(
            std::mem::size_of::<JiredError>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn error_json_shape() {
        let err = JiredError::conflict("stale")
            .with_detail("issue", "PROJ-1")
            .with_hint("run jired pull");
        let json = err.to_json();
        assert_eq!(json["schema"], "jired/error/1");
        assert_eq!(json["code"], 5);
        assert_eq!(json["kind"], "conflict");
        assert_eq!(json["details"]["issue"], "PROJ-1");
        assert_eq!(json["hint"], "run jired pull");
    }
}
