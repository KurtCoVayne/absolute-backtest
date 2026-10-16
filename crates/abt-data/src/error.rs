//! The data layer's errors, and their codes on the wire (P-2.5).

use std::fmt;

use abt_wire::rpc::error;
use abt_wire::{ErrorCode, ErrorMessage};

#[derive(Debug)]
pub enum DataError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Arrow(arrow::error::ArrowError),
    Parquet(parquet::errors::ParquetError),
    /// A schema refused by Definition D-2.1, or rows that do not conform to theirs (D-8.1).
    Schema(String),
    /// An identity check failed (D-8.2).
    Identity(String),
    /// A read found a gap or a reversal in a log's sequence numbers (D-8.3).
    Order(String),
    UnknownRelation(String),
    UnknownVersion(String),
    NotFound(String),
    /// A request the layer does not take, or takes otherwise.
    Malformed(String),
    /// A demand no version of the store satisfies (D-7.4).
    Unresolved(String),
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataError::Io(e) => write!(f, "{}", e),
            DataError::Json(e) => write!(f, "{}", e),
            DataError::Arrow(e) => write!(f, "{}", e),
            DataError::Parquet(e) => write!(f, "{}", e),
            DataError::Schema(m) | DataError::Identity(m) | DataError::Order(m) | DataError::NotFound(m) | DataError::Malformed(m) | DataError::Unresolved(m) => f.write_str(m),
            DataError::UnknownRelation(r) => write!(f, "unknown relation `{}`", r),
            DataError::UnknownVersion(v) => write!(f, "unknown version {}", v),
        }
    }
}

impl std::error::Error for DataError {}

impl From<std::io::Error> for DataError {
    fn from(e: std::io::Error) -> Self {
        DataError::Io(e)
    }
}

impl From<serde_json::Error> for DataError {
    fn from(e: serde_json::Error) -> Self {
        DataError::Json(e)
    }
}

impl From<arrow::error::ArrowError> for DataError {
    fn from(e: arrow::error::ArrowError) -> Self {
        DataError::Arrow(e)
    }
}

impl From<parquet::errors::ParquetError> for DataError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        DataError::Parquet(e)
    }
}

impl DataError {
    /// The error as the protocol carries it.
    pub fn to_wire(&self) -> ErrorMessage {
        let code = match self {
            DataError::UnknownRelation(_) => ErrorCode::UnknownRelation,
            DataError::UnknownVersion(_) => ErrorCode::UnknownVersion,
            DataError::NotFound(_) => ErrorCode::NotFound,
            DataError::Malformed(_) | DataError::Schema(_) => ErrorCode::Malformed,
            DataError::Identity(_) | DataError::Order(_) => ErrorCode::Refused,
            DataError::Unresolved(_) => ErrorCode::Unresolved,
            _ => ErrorCode::Internal,
        };
        error(code, self.to_string())
    }
}
