//! The error type that travels from a query all the way out to an HTTP status code.
//!
//! The Go implementation carries an explicit HTTP status on its `miderr.Err`. We do the same,
//! but as an enum, so that the API layer can map variants to statuses without every call site
//! having to remember which one it meant.

use std::fmt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The caller asked for something nonsensical: a bad interval, an unparseable timestamp,
    /// a range wider than we are willing to serve.
    #[error("{0}")]
    BadRequest(String),

    /// The caller asked for something reasonable that does not exist.
    #[error("{0}")]
    NotFound(String),

    /// We failed. The message is logged; whether it reaches the caller is the API layer's call.
    #[error("{0}")]
    Internal(String),
}

impl Error {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Error::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Error::Internal(msg.into())
    }

    /// The HTTP status this error should be reported as.
    pub fn status_code(&self) -> u16 {
        match self {
            Error::BadRequest(_) => 400,
            Error::NotFound(_) => 404,
            Error::Internal(_) => 500,
        }
    }

    /// Internal errors leak query text, table names and connection strings, so the body we
    /// hand back to an anonymous caller is deliberately vague.
    pub fn public_message(&self) -> String {
        match self {
            Error::BadRequest(m) | Error::NotFound(m) => m.clone(),
            Error::Internal(_) => "Internal Server Error".to_string(),
        }
    }
}

/// Anything that fails deep in a query is an internal error unless someone says otherwise.
impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Error::BadRequest(format!("not an integer: {e}"))
    }
}

impl From<std::num::ParseFloatError> for Error {
    fn from(e: std::num::ParseFloatError) -> Self {
        Error::BadRequest(format!("not a number: {e}"))
    }
}

/// Convenience for building a `BadRequest` with `format!` syntax.
#[macro_export]
macro_rules! bad_request {
    ($($arg:tt)*) => {
        $crate::Error::BadRequest(format!($($arg)*))
    };
}

/// Convenience for building an `Internal` with `format!` syntax.
#[macro_export]
macro_rules! internal_err {
    ($($arg:tt)*) => {
        $crate::Error::Internal(format!($($arg)*))
    };
}

/// Wrapper used when we want the `Display` of an error chain rather than just the outermost
/// message, which is what `{}` on most error types gives you.
pub struct Chain<'a>(pub &'a dyn std::error::Error);

impl fmt::Display for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut src = self.0.source();
        while let Some(e) = src {
            write!(f, ": {e}")?;
            src = e.source();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_match_variants() {
        assert_eq!(Error::bad_request("x").status_code(), 400);
        assert_eq!(Error::not_found("x").status_code(), 404);
        assert_eq!(Error::internal("x").status_code(), 500);
    }

    #[test]
    fn internal_details_are_not_public() {
        let e = Error::internal("SELECT * FROM secrets: connection refused");
        assert_eq!(e.public_message(), "Internal Server Error");
        assert!(e.to_string().contains("connection refused"));
    }

    #[test]
    fn chain_walks_sources() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer")]
        struct Outer(#[source] Inner);
        #[derive(Debug, thiserror::Error)]
        #[error("inner")]
        struct Inner;

        assert_eq!(Chain(&Outer(Inner)).to_string(), "outer: inner");
    }
}
