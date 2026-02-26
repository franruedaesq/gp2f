//! CRDT schema types — available in both `std` and `no_std` environments.
//!
//! These types define the conflict-resolution strategy for each field in a
//! document schema.  They do **not** depend on [`yrs`] or the standard library
//! and are therefore usable on WASM / bare-metal targets that cannot link
//! against `std`.
//!
//! The heavyweight [`super::crdt::CrdtDoc`] wrapper (which requires `yrs` and
//! therefore `std`) is kept in the `crdt` module and gated behind the `"std"`
//! feature.

use serde::{Deserialize, Serialize};

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

/// Per-field conflict resolution strategy registered in the schema.
///
/// Each field in the document schema carries one of these strategies.
/// The server and client agree on the strategy at schema-registration time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldStrategy {
    /// CRDT collaborative text (backed by `yrs::Text` on std targets).
    YjsText,
    /// Last-Write-Wins scalar — no merge needed.
    Lww,
    /// Transactional — reject the whole op if this field conflicts.
    Transactional,
}

/// Schema entry for a single document field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSchema {
    /// JSON-pointer path (e.g. `/notes`).
    pub path: String,
    /// The conflict resolution strategy for this field.
    pub strategy: FieldStrategy,
}

/// A complete document schema: an ordered list of [`FieldSchema`] entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentSchema {
    pub fields: Vec<FieldSchema>,
}

impl DocumentSchema {
    /// Look up the strategy for a given JSON-pointer path.
    /// Falls back to [`FieldStrategy::Lww`] if the path is not registered.
    pub fn strategy_for(&self, path: &str) -> FieldStrategy {
        self.fields
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.strategy)
            .unwrap_or(FieldStrategy::Lww)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_lookup_returns_lww_for_unknown() {
        let schema = DocumentSchema::default();
        assert_eq!(schema.strategy_for("/anything"), FieldStrategy::Lww);
    }

    #[test]
    fn strategy_lookup_finds_registered_field() {
        let schema = DocumentSchema {
            fields: vec![FieldSchema {
                path: "/notes".into(),
                strategy: FieldStrategy::YjsText,
            }],
        };
        assert_eq!(schema.strategy_for("/notes"), FieldStrategy::YjsText);
        assert_eq!(schema.strategy_for("/other"), FieldStrategy::Lww);
    }

    #[test]
    fn field_strategy_serde_round_trip() {
        let s = FieldStrategy::Transactional;
        let json = serde_json::to_string(&s).unwrap();
        let back: FieldStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
