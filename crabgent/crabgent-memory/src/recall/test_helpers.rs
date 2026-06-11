//! Shared `#[cfg(test)]` fixtures for the recall scoring strategies.

use chrono::{DateTime, Utc};
use crabgent_core::{MemoryScope, Owner};
use crabgent_store::{MemoryDoc, MemoryHit};

pub(super) fn doc(class: &str, importance: Option<f32>, created_at: DateTime<Utc>) -> MemoryDoc {
    let mut doc = MemoryDoc::new(MemoryScope::for_owner(Owner::new("u")), class);
    doc.importance = importance;
    doc.created_at = created_at;
    doc.updated_at = created_at;
    doc
}

pub(super) fn hit(doc: &MemoryDoc, score: f32) -> MemoryHit {
    MemoryHit {
        id: doc.id.clone(),
        body: doc.body.clone(),
        score,
        cosine_similarity: None,
        created_at: doc.created_at,
    }
}

pub(super) fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}
