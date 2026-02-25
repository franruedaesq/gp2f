//! CRDT field support for GP2F conflict resolution.
//!
//! Integrates [`yrs`] (Rust port of Yjs) for collaborative-editable fields.
//! Non-CRDT fields use LWW (last-write-wins) or TRANSACTIONAL semantics.

use serde::{Deserialize, Serialize};
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, ReadTxn, Text, TextRef, Transact, Update};

/// Per-field conflict resolution strategy registered in the schema.
///
/// Each field in the document schema carries one of these strategies.
/// The server and client agree on the strategy at schema-registration time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldStrategy {
    /// CRDT collaborative text (backed by `yrs::Text`).
    YjsText,
    /// Last-Write-Wins scalar — no merge needed.
    Lww,
    /// Transactional — reject the whole op if this field conflicts.
    Transactional,
}

/// A lightweight CRDT document wrapper around [`yrs::Doc`].
///
/// One `CrdtDoc` is maintained per CRDT field per (tenant, workflow, instance).
pub struct CrdtDoc {
    inner: Doc,
    field_name: String,
}

impl CrdtDoc {
    /// Create a new empty CRDT document for `field_name`.
    pub fn new(field_name: impl Into<String>) -> Self {
        Self {
            inner: Doc::new(),
            field_name: field_name.into(),
        }
    }

    fn text(&self) -> TextRef {
        self.inner.get_or_insert_text(self.field_name.as_str())
    }

    /// Insert `text` at `index` in the CRDT text field.
    pub fn insert(&self, index: u32, text: &str) {
        let txt = self.text();
        let mut txn = self.inner.transact_mut();
        txt.insert(&mut txn, index, text);
    }

    /// Delete `len` characters starting at `index`.
    pub fn delete(&self, index: u32, len: u32) {
        let txt = self.text();
        let mut txn = self.inner.transact_mut();
        txt.remove_range(&mut txn, index, len);
    }

    /// Return the current string content of the CRDT text field.
    pub fn get_string(&self) -> String {
        let txt = self.text();
        let txn = self.inner.transact();
        txt.get_string(&txn)
    }

    /// Encode the full document state as a binary update (v1 encoding).
    pub fn encode_state(&self) -> Vec<u8> {
        let txn = self.inner.transact();
        txn.encode_state_as_update_v1(&Default::default())
    }

    /// Apply a binary update (v1 encoding) produced by another `CrdtDoc`.
    ///
    /// Returns `Ok(())` on success or an error string on decode failure.
    pub fn apply_update(&self, update: &[u8]) -> Result<(), String> {
        let update = Update::decode_v1(update).map_err(|e| format!("decode error: {e}"))?;
        let mut txn = self.inner.transact_mut();
        txn.apply_update(update)
            .map_err(|e| format!("apply error: {e}"))?;
        Ok(())
    }

    /// Merge `other`'s state into `self` and return the merged string.
    ///
    /// This is the auto-merge path for CRDT fields: the server merges the
    /// client's update into the authoritative document and returns the result.
    pub fn merge_from(&self, other_state: &[u8]) -> Result<String, String> {
        self.apply_update(other_state)?;
        Ok(self.get_string())
    }
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
    fn crdt_insert_and_read() {
        let doc = CrdtDoc::new("notes");
        doc.insert(0, "hello");
        assert_eq!(doc.get_string(), "hello");
    }

    #[test]
    fn crdt_delete() {
        let doc = CrdtDoc::new("notes");
        doc.insert(0, "hello world");
        doc.delete(5, 6); // remove " world"
        assert_eq!(doc.get_string(), "hello");
    }

    #[test]
    fn crdt_merge_two_docs() {
        let a = CrdtDoc::new("notes");
        a.insert(0, "hello");

        let b = CrdtDoc::new("notes");
        b.insert(0, "world");

        // Apply a's full state into b and b's full state into a
        let a_state = a.encode_state();
        let b_state = b.encode_state();

        a.apply_update(&b_state).unwrap();
        b.apply_update(&a_state).unwrap();

        // Both should converge to the same string
        assert_eq!(a.get_string(), b.get_string());
    }

    #[test]
    fn schema_strategy_lookup() {
        let schema = DocumentSchema {
            fields: vec![
                FieldSchema {
                    path: "/notes".into(),
                    strategy: FieldStrategy::YjsText,
                },
                FieldSchema {
                    path: "/amount".into(),
                    strategy: FieldStrategy::Transactional,
                },
            ],
        };
        assert_eq!(schema.strategy_for("/notes"), FieldStrategy::YjsText);
        assert_eq!(schema.strategy_for("/amount"), FieldStrategy::Transactional);
        assert_eq!(schema.strategy_for("/unknown"), FieldStrategy::Lww);
    }

    #[test]
    fn field_strategy_serde_round_trip() {
        let s = FieldStrategy::YjsText;
        let json = serde_json::to_string(&s).unwrap();
        let back: FieldStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
