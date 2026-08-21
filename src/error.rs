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

/// Wrap an i/o failure with the path it happened to.
///
/// The blanket `From<io::Error>` cannot know the path, and "No such file or
/// directory" without a filename is close to useless. Every `fs::` call site
/// that knows its path should route through this instead of a bare `?`.
pub fn io_error(what: &str, path: impl AsRef<std::path::Path>, e: std::io::Error) -> JiredError {
    let kind = match e.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound => ErrorKind::Usage,
        _ => ErrorKind::Generic,
    };
    let err = JiredError::new(
        kind,
        format!("cannot {what} {}: {e}", path.as_ref().display()),
    )
    .with_detail("path", path.as_ref().display().to_string());
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            err.with_hint("check the file's owner and permissions")
        }
        _ => err,
    }
}

/// Convenience for `map_err` at an `fs::` call site.
pub fn io_ctx(
    what: &'static str,
    path: impl AsRef<std::path::Path>,
) -> impl FnOnce(std::io::Error) -> JiredError {
    let path = path.as_ref().to_path_buf();
    move |e| io_error(what, path, e)
}

impl From<std::io::Error> for JiredError {
    fn from(e: std::io::Error) -> Self {
        // The message stays context-free: `Display` appends the source, and
        // interpolating `{e}` here too would print the cause twice.
        JiredError::generic("i/o error").with_source(e)
    }
}

impl From<rusqlite::Error> for JiredError {
    fn from(e: rusqlite::Error) -> Self {
        // Not every SQLite failure is corruption. A busy database means another
        // process, a read-only one means the filesystem — telling the user their
        // state is corrupt would send them to delete a database that is fine.
        use rusqlite::ffi::ErrorCode;
        let (kind, hint) = match &e {
            rusqlite::Error::SqliteFailure(f, _) => match f.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => (
                    ErrorKind::Locked,
                    Some("another jired process is using this workspace; wait for it to finish"),
                ),
                ErrorCode::ReadOnly | ErrorCode::CannotOpen | ErrorCode::PermissionDenied => (
                    ErrorKind::Generic,
                    Some("check that .jired/ is writable by you"),
                ),
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => (
                    ErrorKind::CorruptState,
                    Some("run `jired doctor`; if it is state.db, deleting it and running `jired fetch` rebuilds it"),
                ),
                _ => (ErrorKind::CorruptState, None),
            },
            _ => (ErrorKind::CorruptState, None),
        };
        let err = JiredError::new(kind, "database error").with_source(e);
        match hint {
            Some(h) => err.with_hint(h),
            None => err,
        }
    }
}

impl From<serde_json::Error> for JiredError {
    fn from(e: serde_json::Error) -> Self {
        JiredError::generic("json error").with_source(e)
    }
}

impl From<serde_yaml_ng::Error> for JiredError {
    fn from(e: serde_yaml_ng::Error) -> Self {
        JiredError::generic("yaml error").with_source(e)
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

    /// `Display` appends the source, so a blanket conversion must not also
    /// interpolate it — that printed every i/o cause twice.
    #[test]
    fn a_converted_error_states_its_cause_once() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err: JiredError = io.into();
        let text = err.to_string();
        assert_eq!(text.matches("no such file").count(), 1, "{text}");
    }

    /// Not every SQLite failure is corruption; saying so sends people to delete
    /// a database that is fine.
    #[test]
    fn sqlite_failures_keep_their_own_kinds() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(5), // SQLITE_BUSY
            Some("database is locked".into()),
        );
        assert_eq!(JiredError::from(busy).exit_code(), ExitCode::Locked);

        let corrupt = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(11), // SQLITE_CORRUPT
            Some("malformed".into()),
        );
        let err = JiredError::from(corrupt);
        assert_eq!(err.exit_code(), ExitCode::CorruptState);
        assert!(err.hint.is_some(), "corruption must say how to recover");
    }

    /// A pathless "No such file or directory" is close to useless.
    #[test]
    fn io_errors_name_the_path_they_happened_to() {
        let err = io_error(
            "read",
            "/tmp/PROJ-1.md",
            std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        );
        assert!(err.message.contains("/tmp/PROJ-1.md"), "{}", err.message);
        assert_eq!(err.details["path"], "/tmp/PROJ-1.md");
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
