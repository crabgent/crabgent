//! Tests for [`crate::ops::relations`] (split out to keep `relations.rs`
//! under the 500-line cap).

use std::sync::Arc;

use crabgent_core::policy::strict::{ActionMatcher, Rule, StrictPolicy};
use crabgent_core::{MemoryId, MemoryScope, Owner, Tool, ToolError};
use crabgent_store::{MemoryDoc, MemoryRelation, MemoryStore, RelationType};
use serde_json::{Value, json};

use crate::ops::test_support::{alice_ctx, alice_scope, alice_scope_value, allow_all_tool};
use crate::{MemoryTool, ops::test_support::make_tool};

async fn store_doc(store: &Arc<crabgent_store::MemoryMemoryStore>, body: &str) -> MemoryId {
    store
        .store(&MemoryDoc::new(alice_scope(), body))
        .await
        .expect("store doc")
}

fn store_args(from: &MemoryId, to: &MemoryId, rt: &str) -> Value {
    json!({
        "op": "relation_store",
        "scope": alice_scope_value(),
        "from_id": from.to_string(),
        "to_id": to.to_string(),
        "relation_type": rt,
    })
}

async fn link(tool: &MemoryTool, from: &MemoryId, to: &MemoryId, rt: &str) {
    tool.execute(store_args(from, to, rt), &alice_ctx())
        .await
        .expect("link edge");
}

#[tokio::test]
async fn relation_store_returns_id_and_endpoints() {
    let (tool, store) = allow_all_tool();
    let from = store_doc(&store, "from").await;
    let to = store_doc(&store, "to").await;
    let res = tool
        .execute(store_args(&from, &to, "relates_to"), &alice_ctx())
        .await
        .expect("relation store");
    assert!(res["relation_id"].as_str().is_some());
    assert_eq!(res["from_id"], from.to_string());
    assert_eq!(res["to_id"], to.to_string());
    assert_eq!(res["relation_type"], "relates_to");
}

#[tokio::test]
async fn relation_store_denied_yields_permission_error() {
    // StrictPolicy that only grants relation_delete: relation_store is denied.
    let policy = Arc::new(StrictPolicy::builder().allow_relation_delete().build());
    let (tool, store) = make_tool(policy);
    let from = store_doc(&store, "from").await;
    let to = store_doc(&store, "to").await;
    let err = tool
        .execute(store_args(&from, &to, "relates_to"), &alice_ctx())
        .await
        .expect_err("expected deny");
    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn relation_store_raw_matcher_policy_also_denies_expand() {
    // Sanity: a policy granting only relation_store denies relation_expand.
    let policy = Arc::new(
        StrictPolicy::builder()
            .rule(Rule::allow(ActionMatcher::RelationStore).requires_scope_from_subject())
            .build(),
    );
    let (tool, store) = make_tool(policy);
    let root = store_doc(&store, "root").await;
    let err = tool
        .execute(
            json!({
                "op": "relation_expand",
                "scope": alice_scope_value(),
                "from_id": root.to_string(),
            }),
            &alice_ctx(),
        )
        .await
        .expect_err("expected deny");
    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn relation_delete_returns_deleted_bool() {
    let (tool, store) = allow_all_tool();
    let from = store_doc(&store, "from").await;
    let to = store_doc(&store, "to").await;
    link(&tool, &from, &to, "relates_to").await;
    let del = tool
        .execute(
            json!({
                "op": "relation_delete",
                "scope": alice_scope_value(),
                "from_id": from.to_string(),
                "to_id": to.to_string(),
                "relation_type": "relates_to",
            }),
            &alice_ctx(),
        )
        .await
        .expect("relation delete");
    assert_eq!(del["deleted"], true);
}

#[tokio::test]
async fn relation_delete_returns_false_for_missing_edge() {
    let (tool, store) = allow_all_tool();
    let from = store_doc(&store, "from").await;
    let to = store_doc(&store, "to").await;
    let del = tool
        .execute(
            json!({
                "op": "relation_delete",
                "scope": alice_scope_value(),
                "from_id": from.to_string(),
                "to_id": to.to_string(),
                "relation_type": "relates_to",
            }),
            &alice_ctx(),
        )
        .await
        .expect("relation delete");
    assert_eq!(del["deleted"], false);
}

#[tokio::test]
async fn relation_expand_bounded_by_depth_three() {
    let (tool, store) = allow_all_tool();
    let root = store_doc(&store, "root").await;
    let a = store_doc(&store, "a").await;
    let b = store_doc(&store, "b").await;
    let c = store_doc(&store, "c").await;
    let d = store_doc(&store, "d").await;
    link(&tool, &root, &a, "next").await;
    link(&tool, &a, &b, "next").await;
    link(&tool, &b, &c, "next").await;
    link(&tool, &c, &d, "next").await;

    let res = tool
        .execute(
            json!({
                "op": "relation_expand",
                "scope": alice_scope_value(),
                "from_id": root.to_string(),
                "depth": 3,
            }),
            &alice_ctx(),
        )
        .await
        .expect("relation expand");

    let nodes: Vec<&str> = res["node_ids"]
        .as_array()
        .expect("node_ids array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(nodes.contains(&c.to_string().as_str()), "depth 3 reaches c");
    assert!(
        !nodes.contains(&d.to_string().as_str()),
        "depth 3 excludes d"
    );
    assert_eq!(res["truncated"], false);
    // root->a (1), a->b (2), b->c (3): exactly three edges, none to d.
    let edges = res["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 3);
    assert!(
        edges
            .iter()
            .any(|e| e["depth"] == 3 && e["to_id"] == c.to_string())
    );
}

#[tokio::test]
async fn relation_expand_depth_clamped_to_three() {
    let (tool, store) = allow_all_tool();
    let root = store_doc(&store, "root").await;
    let a = store_doc(&store, "a").await;
    let b = store_doc(&store, "b").await;
    let c = store_doc(&store, "c").await;
    let d = store_doc(&store, "d").await;
    link(&tool, &root, &a, "next").await;
    link(&tool, &a, &b, "next").await;
    link(&tool, &b, &c, "next").await;
    link(&tool, &c, &d, "next").await;

    let res = tool
        .execute(
            json!({
                "op": "relation_expand",
                "scope": alice_scope_value(),
                "from_id": root.to_string(),
                "depth": 99,
            }),
            &alice_ctx(),
        )
        .await
        .expect("relation expand");
    let reaches_d = res["node_ids"]
        .as_array()
        .expect("node_ids array")
        .iter()
        .filter_map(Value::as_str)
        .any(|id| id == d.to_string());
    assert!(!reaches_d, "clamp keeps d out");
}

#[tokio::test]
async fn relation_expand_cycle_terminates() {
    let (tool, store) = allow_all_tool();
    let root = store_doc(&store, "root").await;
    let a = store_doc(&store, "a").await;
    link(&tool, &root, &a, "next").await;
    link(&tool, &a, &root, "back").await;

    let res = tool
        .execute(
            json!({
                "op": "relation_expand",
                "scope": alice_scope_value(),
                "from_id": root.to_string(),
                "depth": 3,
            }),
            &alice_ctx(),
        )
        .await
        .expect("relation expand terminates on cycle");
    // Two distinct edges (root->a, a->root); only two nodes total.
    let nodes = res["node_ids"].as_array().expect("node_ids array");
    assert_eq!(nodes.len(), 2);
    let edges = res["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 2);
    assert_eq!(res["truncated"], false);
}

#[tokio::test]
async fn relation_expand_dedups_repeated_edge() {
    // relation_neighbors returns the same edge for both endpoints in a
    // frontier; the edge must be recorded once.
    let (tool, store) = allow_all_tool();
    let root = store_doc(&store, "root").await;
    let a = store_doc(&store, "a").await;
    let b = store_doc(&store, "b").await;
    // root linked to both a and b; a linked to b. At depth 2 the frontier
    // is [a, b] and the a->b edge is incident to both.
    link(&tool, &root, &a, "next").await;
    link(&tool, &root, &b, "next").await;
    link(&tool, &a, &b, "cross").await;

    let res = tool
        .execute(
            json!({
                "op": "relation_expand",
                "scope": alice_scope_value(),
                "from_id": root.to_string(),
                "depth": 3,
            }),
            &alice_ctx(),
        )
        .await
        .expect("relation expand");
    let edges = res["edges"].as_array().expect("edges array");
    let cross_edges = edges
        .iter()
        .filter(|e| e["relation_type"] == "cross")
        .count();
    assert_eq!(cross_edges, 1, "a->b cross edge recorded exactly once");
}

#[tokio::test]
async fn invalid_relation_type_yields_arg_error() {
    let (tool, store) = allow_all_tool();
    let from = store_doc(&store, "from").await;
    let to = store_doc(&store, "to").await;
    let err = tool
        .execute(store_args(&from, &to, "has-dash"), &alice_ctx())
        .await
        .expect_err("invalid relation_type");
    assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains("relation_type")));
}

#[tokio::test]
async fn invalid_from_id_yields_arg_error() {
    let (tool, _) = allow_all_tool();
    let to = MemoryId::new();
    let err = tool
        .execute(
            json!({
                "op": "relation_store",
                "scope": alice_scope_value(),
                "from_id": "not-a-uuid",
                "to_id": to.to_string(),
                "relation_type": "relates_to",
            }),
            &alice_ctx(),
        )
        .await
        .expect_err("invalid from_id");
    assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains("from_id")));
}

#[tokio::test]
async fn relation_store_cross_owner_endpoint_is_invalid_args() {
    // alice references a doc owned by bob. The store rejects the non-visible
    // endpoint as NotFound, which the tool maps to a recoverable arg error so
    // the LLM cannot use the call as a cross-owner existence oracle.
    let (tool, store) = allow_all_tool();
    let mine = store_doc(&store, "mine").await;
    let bob = store
        .store(&MemoryDoc::new(
            MemoryScope::for_owner(Owner::new("bob")),
            "bob doc",
        ))
        .await
        .expect("store bob doc");

    let err = tool
        .execute(store_args(&mine, &bob, "relates_to"), &alice_ctx())
        .await
        .expect_err("cross-owner endpoint rejected");
    assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains("does not reference")));
}

#[tokio::test]
async fn relation_store_missing_doc_is_invalid_args() {
    // relation_store on absent docs maps NotFound to a recoverable arg error,
    // not a backend Execution error, so the LLM gets an actionable message.
    let (tool, _) = allow_all_tool();
    let from = MemoryId::new();
    let to = MemoryId::new();
    let err = tool
        .execute(store_args(&from, &to, "relates_to"), &alice_ctx())
        .await
        .expect_err("missing docs");
    assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains("does not reference")));
}

#[tokio::test]
async fn relation_neighbors_helper_is_exercised_directly() {
    // Direct store-level sanity that the in-memory backend returns the edge.
    let (tool, store) = allow_all_tool();
    let from = store_doc(&store, "from").await;
    let to = store_doc(&store, "to").await;
    link(&tool, &from, &to, "relates_to").await;
    let rt = RelationType::new("relates_to").expect("relation type");
    let stored = MemoryRelation::new(from.clone(), to.clone(), rt, alice_scope());
    // Re-store is idempotent: same edge, same id.
    let first = store
        .relation_store(&stored)
        .await
        .expect("idempotent store");
    let second = store
        .relation_store(&stored)
        .await
        .expect("idempotent store");
    assert_eq!(first.to_string(), second.to_string());
}
