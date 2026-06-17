//! `{name}@{revision}` embedding-model identifier wire format (D12, FR-038, FR-039).
//!
//! Every cloud `/v1/search` request, every chunk read response, and every MCP
//! tool result carries an [`EmbeddingModelId`]. A mismatch between the client's
//! local model and the corpus's active model is detected at the API boundary and
//! returns `409 embedding_model_mismatch` (see
//! [`crate::error::ErrorCode::EmbeddingModelMismatch`]).
//!
//! Wire grammar: `name`, then literal `@`, then a positive integer revision.
//! Examples: `"voyage-context-3@1"`, `"voyage-code-3@2"`. Empty names and
//! non-positive or non-numeric revisions are rejected.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable identifier for one row in the `embedding_model` table.
///
/// The revision is a monotonic integer minted by the server on registration;
/// it intentionally does NOT mirror upstream model versions (which are semver
/// and can be re-released) — revisions are stable within our corpus.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmbeddingModelId {
    /// The model's canonical name as published by its provider (e.g. `bge-base-en-v1.5`).
    pub name: String,
    /// Monotonic per-name revision. Always `>= 1`.
    pub revision: u32,
}

impl EmbeddingModelId {
    /// Construct an id without going through the string form.
    ///
    /// # Errors
    ///
    /// Returns [`ParseEmbeddingModelIdError::EmptyName`] if `name` is empty,
    /// [`ParseEmbeddingModelIdError::NameContainsAt`] if `name` contains `@`,
    /// or [`ParseEmbeddingModelIdError::InvalidRevision`] if `revision == 0`.
    pub fn new(name: impl Into<String>, revision: u32) -> Result<Self, ParseEmbeddingModelIdError> {
        let name = name.into();
        if name.is_empty() {
            return Err(ParseEmbeddingModelIdError::EmptyName);
        }
        if name.contains('@') {
            return Err(ParseEmbeddingModelIdError::NameContainsAt);
        }
        if revision == 0 {
            return Err(ParseEmbeddingModelIdError::InvalidRevision("0".to_owned()));
        }
        Ok(Self { name, revision })
    }
}

impl fmt::Display for EmbeddingModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.revision)
    }
}

impl FromStr for EmbeddingModelId {
    type Err = ParseEmbeddingModelIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, rev) = s
            .split_once('@')
            .ok_or(ParseEmbeddingModelIdError::MissingAtSeparator)?;
        if name.is_empty() {
            return Err(ParseEmbeddingModelIdError::EmptyName);
        }
        if name.contains('@') {
            return Err(ParseEmbeddingModelIdError::NameContainsAt);
        }
        let revision: u32 = rev
            .parse()
            .map_err(|_| ParseEmbeddingModelIdError::InvalidRevision(rev.to_owned()))?;
        if revision == 0 {
            return Err(ParseEmbeddingModelIdError::InvalidRevision(rev.to_owned()));
        }
        Ok(Self {
            name: name.to_owned(),
            revision,
        })
    }
}

impl Serialize for EmbeddingModelId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EmbeddingModelId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// All the ways an [`EmbeddingModelId`] parse can fail.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseEmbeddingModelIdError {
    /// The `@` separator was not found in the string form.
    #[error("missing `@` separator (expected `name@revision`)")]
    MissingAtSeparator,
    /// `name` portion was empty.
    #[error("model name is empty")]
    EmptyName,
    /// `name` portion contained an `@` (only the separator may use `@`).
    #[error("model name contains `@` (use the separator)")]
    NameContainsAt,
    /// Revision was 0, negative, non-numeric, or out of `u32` range.
    #[error("invalid revision `{0}` (expected positive integer)")]
    InvalidRevision(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_form() {
        let id: EmbeddingModelId = "bge-base-en-v1.5@1".parse().unwrap();
        assert_eq!(id.name, "bge-base-en-v1.5");
        assert_eq!(id.revision, 1);
    }

    #[test]
    fn round_trips() {
        let id = EmbeddingModelId::new("voyage-code-3", 42).unwrap();
        let s = id.to_string();
        let back: EmbeddingModelId = s.parse().unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn rejects_missing_at() {
        assert_eq!(
            "bge-base-en-v1.5".parse::<EmbeddingModelId>(),
            Err(ParseEmbeddingModelIdError::MissingAtSeparator),
        );
    }

    #[test]
    fn rejects_empty_name() {
        assert_eq!("@1".parse::<EmbeddingModelId>(), Err(ParseEmbeddingModelIdError::EmptyName),);
    }

    #[test]
    fn rejects_zero_revision() {
        match "name@0".parse::<EmbeddingModelId>() {
            Err(ParseEmbeddingModelIdError::InvalidRevision(s)) => assert_eq!(s, "0"),
            other => panic!("expected InvalidRevision, got {other:?}"),
        }
    }

    #[test]
    fn rejects_negative_revision() {
        assert!(matches!(
            "name@-1".parse::<EmbeddingModelId>(),
            Err(ParseEmbeddingModelIdError::InvalidRevision(_)),
        ));
    }

    #[test]
    fn rejects_non_numeric_revision() {
        assert!(matches!(
            "name@v1".parse::<EmbeddingModelId>(),
            Err(ParseEmbeddingModelIdError::InvalidRevision(_)),
        ));
    }

    #[test]
    fn rejects_overflow_revision() {
        let big = format!("name@{}", u64::from(u32::MAX) + 1);
        assert!(matches!(
            big.parse::<EmbeddingModelId>(),
            Err(ParseEmbeddingModelIdError::InvalidRevision(_)),
        ));
    }

    #[test]
    fn serde_uses_string_form() {
        let id = EmbeddingModelId::new("bge-base-en-v1.5", 1).unwrap();
        let j = serde_json::to_value(&id).unwrap();
        assert_eq!(j, serde_json::Value::String("bge-base-en-v1.5@1".into()));
        let back: EmbeddingModelId = serde_json::from_value(j).unwrap();
        assert_eq!(id, back);
    }
}
