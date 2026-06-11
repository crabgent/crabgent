//! Memory relation operations: store, delete, and bounded BFS expand.

use std::collections::HashSet;
use std::str::FromStr;

use crabgent_core::error::ToolError;
use crabgent_core::tool::ToolCtx;
use crabgent_core::{Action, MemoryId, MemoryScope};
use crabgent_store::{MemoryRelation, RelationType, StoreError};
use serde_json::{Value, json};

use crate::MemoryTool;
use crate::ops::Args;
use crate::store_unavailable;

/// Maximum BFS hop count for `relation_expand`. The op self-caps at this depth
/// even if the caller requests more (documented in the tool description).
const MAX_RELATION_DEPTH: u32 = 3;

/// Hard cap on the number of distinct nodes visited during a single
/// `relation_expand`. Hitting the cap sets `truncated: true` and emits a
/// `warn!` rather than silently dropping edges.
const MAX_RELATION_NODES: usize = 128;

pub async fn do_relation_store(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let from = parse_id(args.from_id.as_deref(), "from_id", "relation_store")?;
    let to = parse_id(args.to_id.as_deref(), "to_id", "relation_store")?;
    let rt = parse_relation_type(args, "relation_store")?;
    let action = Action::RelationStore {
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;
    let relation = MemoryRelation::new(from.clone(), to.clone(), rt.clone(), scope);
    let id = tool
        .store
        .relation_store(&relation)
        .await
        .map_err(|err| match err {
            // A missing endpoint is a caller mistake (referencing a deleted or
            // non-existent doc), not a backend outage. Surface it as a
            // recoverable arg error so the LLM gets an actionable message.
            StoreError::NotFound => ToolError::InvalidArgs(
                "from_id or to_id does not reference an existing memory document".to_owned(),
            ),
            other => store_unavailable("memory.relation_store", &other),
        })?;
    Ok(json!({
        "relation_id": id.to_string(),
        "from_id": from.to_string(),
        "to_id": to.to_string(),
        "relation_type": rt.as_str(),
    }))
}

pub async fn do_relation_delete(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let from = parse_id(args.from_id.as_deref(), "from_id", "relation_delete")?;
    let to = parse_id(args.to_id.as_deref(), "to_id", "relation_delete")?;
    let rt = parse_relation_type(args, "relation_delete")?;
    let action = Action::RelationDelete {
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;
    let deleted = tool
        .store
        .relation_delete(&from, &to, &rt, &scope)
        .await
        .map_err(|err| store_unavailable("memory.relation_delete", &err))?;
    Ok(json!({ "deleted": deleted }))
}

pub async fn do_relation_expand(
    tool: &MemoryTool,
    args: &Args,
    scope: MemoryScope,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let root = parse_id(args.from_id.as_deref(), "from_id", "relation_expand")?;
    let depth = args
        .depth
        .unwrap_or(MAX_RELATION_DEPTH)
        .min(MAX_RELATION_DEPTH);
    let action = Action::RelationExpand {
        scope: scope.clone(),
    };
    tool.gate(&action, ctx).await?;

    let mut walk = RelationWalk::new(root.clone());
    for current_depth in 1..=depth {
        if walk.frontier.is_empty() || walk.cap_reached {
            break;
        }
        let neighbors = tool
            .store
            .relation_neighbors(&walk.frontier, &scope)
            .await
            .map_err(|err| store_unavailable("memory.relation_expand", &err))?;
        walk.absorb_hop(&neighbors, current_depth);
    }
    if walk.cap_reached {
        crabgent_log::warn!(
            op = "memory.relation_expand",
            node_cap = MAX_RELATION_NODES,
            "relation expand truncated at node cap",
        );
    }

    Ok(json!({
        "root": root.to_string(),
        "edges": walk.edges,
        "node_ids": walk.node_ids(),
        "truncated": walk.cap_reached,
    }))
}

/// Mutable BFS state for `relation_expand`. Tracks visited nodes (cycle guard),
/// recorded edges, edge-dedup keys, the current frontier, and whether the node
/// cap was hit.
struct RelationWalk {
    visited: HashSet<MemoryId>,
    edge_keys: HashSet<(MemoryId, MemoryId, String)>,
    frontier: Vec<MemoryId>,
    edges: Vec<Value>,
    cap_reached: bool,
}

impl RelationWalk {
    fn new(root: MemoryId) -> Self {
        let mut visited = HashSet::new();
        visited.insert(root.clone());
        Self {
            visited,
            edge_keys: HashSet::new(),
            frontier: vec![root],
            edges: Vec::new(),
            cap_reached: false,
        }
    }

    /// Process one hop of neighbors at `current_depth`: record each new edge
    /// once and collect not-yet-visited endpoints as the next frontier. Stops
    /// admitting new nodes once the node cap is reached.
    fn absorb_hop(&mut self, neighbors: &[MemoryRelation], current_depth: u32) {
        let mut next = Vec::new();
        for edge in neighbors {
            self.record_edge(edge, current_depth);
            self.enqueue_endpoint(&edge.from_id, &mut next);
            self.enqueue_endpoint(&edge.to_id, &mut next);
        }
        self.frontier = next;
    }

    fn record_edge(&mut self, edge: &MemoryRelation, current_depth: u32) {
        let key = (
            edge.from_id.clone(),
            edge.to_id.clone(),
            edge.relation_type.as_str().to_owned(),
        );
        if self.edge_keys.insert(key) {
            self.edges.push(json!({
                "from_id": edge.from_id.to_string(),
                "to_id": edge.to_id.to_string(),
                "relation_type": edge.relation_type.as_str(),
                "depth": current_depth,
            }));
        }
    }

    fn enqueue_endpoint(&mut self, id: &MemoryId, next: &mut Vec<MemoryId>) {
        if self.cap_reached || self.visited.contains(id) {
            return;
        }
        if self.visited.len() >= MAX_RELATION_NODES {
            self.cap_reached = true;
            return;
        }
        self.visited.insert(id.clone());
        next.push(id.clone());
    }

    fn node_ids(&self) -> Vec<String> {
        self.visited.iter().map(MemoryId::to_string).collect()
    }
}

fn parse_id(raw: Option<&str>, field: &str, op: &str) -> Result<MemoryId, ToolError> {
    let value =
        raw.ok_or_else(|| ToolError::InvalidArgs(format!("{field} required for op={op}")))?;
    MemoryId::from_str(value).map_err(|e| ToolError::InvalidArgs(format!("{field}: {e}")))
}

fn parse_relation_type(args: &Args, op: &str) -> Result<RelationType, ToolError> {
    let raw = args
        .relation_type
        .as_deref()
        .ok_or_else(|| ToolError::InvalidArgs(format!("relation_type required for op={op}")))?;
    RelationType::new(raw).map_err(|e| ToolError::InvalidArgs(format!("relation_type: {e}")))
}

#[cfg(test)]
#[path = "relations_tests.rs"]
mod tests;
