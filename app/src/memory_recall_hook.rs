//! Auto-inject memory hits matching the latest user message.
//!
//! Fires on the first `before_llm` call of a run. Embeds the last
//! user-message text, runs a scope-narrowed `MemoryStore::search`,
//! and prefixes the matched memories into the *content* of the last
//! user message itself. This keeps:
//!
//! * the system prompt unchanged → Codex prompt-cache prefix stays
//!   stable (`cache_read_tokens` > 0 for the static instructions
//!   region),
//! * the role sequence valid for strict
//!   user/assistant alternation) and `OpenAI` (anything goes),
//! * no extra synthetic turns that would confuse provider semantics.
//!
//! Subsequent `before_llm` callbacks within the same run are skipped
//! via a per-`RunId` fired-flag; tool-loop continuations (last message
//! = `tool_result`) are skipped entirely so injected memories appear
//! only once per user turn.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    Decision, EmbeddingProvider, EmbeddingRequest, Hook, LlmRequest, MemoryScope, Outcome, Owner,
    RunCtx, RunId, SearchQuery, Subject,
};
use crabgent_log::{info, warn};
use crabgent_memory::MemoryClass;
use crabgent_store::{MemoryHit, MemoryStore};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::agent_message::ORIGIN_OWNER_ATTR;
use crate::config::UserIdentity;

const DEFAULT_LIMIT: u32 = 15;
const BODY_CAP: usize = 400;
const PINNED_LIMIT: u32 = 30;

/// Classes pulled by the per-user cosine recall. Deliberately excludes
/// `Recency` (volatile, churns every turn) and the pinned classes
/// below (fetched wholesale, not by similarity).
const RECALL_CLASSES: &[MemoryClass] = &[MemoryClass::Semantic, MemoryClass::Episodic];

/// Pinned per-user: identity + freeform notes. Owner-scoped so one
/// human's profile never surfaces for another.
const USER_PINNED_CLASSES: &[MemoryClass] = &[MemoryClass::UserProfile, MemoryClass::Notes];

/// Pinned agent-shared (owner-less): visible to every human and every
/// channel the agent serves.
const SHARED_PINNED_CLASSES: &[MemoryClass] = &[MemoryClass::Skill, MemoryClass::Tools];

pub struct MemoryRecallHook {
    store: Arc<dyn MemoryStore>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    limit: u32,
    /// Maps any one owner string to the full owner-string set of the
    /// canonical user it belongs to, so recall for a Matrix MXID also
    /// returns memories stored under that human's Telegram id. Owners
    /// absent from the map resolve to themselves (isolation default).
    owner_map: HashMap<String, Vec<String>>,
    fired: Arc<Mutex<HashSet<RunId>>>,
}

impl MemoryRecallHook {
    pub fn new(
        store: Arc<dyn MemoryStore>,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        limit: u32,
        users: &[UserIdentity],
    ) -> Self {
        let mut owner_map: HashMap<String, Vec<String>> = HashMap::new();
        for user in users {
            for owner in &user.owners {
                owner_map.insert(owner.clone(), user.owners.clone());
            }
        }
        Self {
            store,
            embedder,
            limit: if limit == 0 { DEFAULT_LIMIT } else { limit },
            owner_map,
            fired: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Owner strings to recall for the current subject. Delegates to
    /// [`resolve_recall_owners`]; see there for the impersonation rule.
    fn recall_owners(&self, subject: &Subject) -> Vec<String> {
        resolve_recall_owners(&self.owner_map, subject)
    }

    /// Embed the user turn once. Returns `None` when no embedder is
    /// wired or the call fails; recall then degrades to FTS-only.
    async fn embed_query(&self, query_text: &str, ctx: &RunCtx) -> Option<Vec<f32>> {
        let embedder = self.embedder.as_ref()?;
        let request = EmbeddingRequest {
            texts: vec![query_text.to_owned()],
            model: None,
        };
        match embedder.embed(request, ctx, Some(&ctx.cancel)).await {
            Ok(resp) => resp.vectors.into_iter().next(),
            Err(err) => {
                warn!(error = %err, "memory-recall: embed query failed; FTS-only");
                None
            }
        }
    }

    /// Run one scoped, class-filtered search. Attaches the shared query
    /// embedding when present. Errors degrade to an empty result.
    async fn search_class(
        &self,
        scope: MemoryScope,
        class: MemoryClass,
        limit: u32,
        embedding: Option<&Vec<f32>>,
        include_shared: bool,
    ) -> Vec<MemoryHit> {
        let mut q = SearchQuery::new(String::new())
            .scope(scope)
            .class(class.as_str())
            .limit(limit)
            .include_shared(include_shared);
        if let Some(vector) = embedding {
            q = q.embedding(vector.clone());
        }
        match self.store.search(&q).await {
            Ok(hits) => hits,
            Err(err) => {
                warn!(error = %err, class = class.as_str(), "memory-recall: search failed");
                Vec::new()
            }
        }
    }
}

/// Agent + single-owner scope (private recall). Channel/conv/kind are
/// intentionally left unset so a memory stored in one channel surfaces
/// in every channel the same human uses.
fn user_scope(agent: Option<&str>, owner: &str) -> MemoryScope {
    let mut scope = MemoryScope::global();
    scope.agent = agent.map(str::to_owned);
    scope.owner = Some(Owner::new(owner));
    scope
}

/// Agent-only scope (shared recall): matches owner-less skill/tool rows
/// regardless of which human is talking.
fn shared_scope(agent: Option<&str>) -> MemoryScope {
    let mut scope = MemoryScope::global();
    scope.agent = agent.map(str::to_owned);
    scope
}

/// Owner strings to recall for a subject.
///
/// Read-only impersonation: a relayed agent-to-agent run carries the
/// originating human's owner in [`ORIGIN_OWNER_ATTR`], so a peer agent
/// recalls the human's memory instead of its own `agent:<name>` pseudo-owner.
/// A normal channel run has no such attr and the subject's own id is the
/// owner. Either key is then expanded through the canonical-user map when
/// present, else used as-is (the
/// isolation default).
fn resolve_recall_owners(
    owner_map: &HashMap<String, Vec<String>>,
    subject: &Subject,
) -> Vec<String> {
    let key = subject
        .attr(ORIGIN_OWNER_ATTR)
        .unwrap_or_else(|| subject.id());
    owner_map
        .get(key)
        .cloned()
        .unwrap_or_else(|| vec![key.to_owned()])
}

#[async_trait]
impl Hook for MemoryRecallHook {
    #[allow(clippy::too_many_lines)]
    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        {
            let mut fired = self.fired.lock().await;
            if fired.contains(&ctx.run_id) {
                return Decision::Continue;
            }
            fired.insert(ctx.run_id.clone());
        }

        let tail_role = req
            .messages
            .last()
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("none");
        info!(
            run_id = %ctx.run_id,
            message_count = req.messages.len(),
            tail_role,
            "memory-recall: invoked",
        );

        let Some(query_text) = extract_last_user_text(&req.messages) else {
            info!(
                run_id = %ctx.run_id,
                tail_role,
                "memory-recall: skip (trailing message is not user-text)",
            );
            return Decision::Continue;
        };
        if query_text.trim().is_empty() {
            info!(run_id = %ctx.run_id, "memory-recall: skip (empty query text)");
            return Decision::Continue;
        }

        // Embed the user turn once; reused across every scoped search.
        // The SQLite hybrid path uses FTS as a hard pre-filter, so an
        // empty FTS string plus this vector ranks the scope purely by
        // cosine distance.
        let embedding = self.embed_query(&query_text, ctx).await;
        let owners = self.recall_owners(&ctx.subject);
        let agent = ctx.subject.attr("agent");

        // Per-user cosine recall across semantic + episodic only. One
        // search per (owner, class); merge keeping the best score per id
        // so a memory shared across a human's channel identities is not
        // double-counted.
        let mut by_id: HashMap<String, MemoryHit> = HashMap::new();
        for owner in &owners {
            for class in RECALL_CLASSES {
                for hit in self
                    .search_class(
                        user_scope(agent, owner),
                        *class,
                        self.limit,
                        embedding.as_ref(),
                        // Pull the agent's shared (owner-less) rows alongside
                        // this user's private semantic/episodic memories.
                        true,
                    )
                    .await
                {
                    let id = hit.id.to_string();
                    by_id
                        .entry(id)
                        .and_modify(|e| {
                            if hit.score > e.score {
                                *e = hit.clone();
                            }
                        })
                        .or_insert(hit);
                }
            }
        }
        let mut hits: Vec<MemoryHit> = by_id.into_values().collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(self.limit as usize);

        // Pinned blocks fetched wholesale (not by similarity): stable
        // identity must always be in front of the LLM. user_profile +
        // notes are per-user; skill + tools are agent-shared so every
        // human and channel sees them.
        let mut pinned: Vec<MemoryHit> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> =
            hits.iter().map(|h| h.id.to_string()).collect();
        for owner in &owners {
            for class in USER_PINNED_CLASSES {
                for hit in self
                    .search_class(user_scope(agent, owner), *class, PINNED_LIMIT, None, false)
                    .await
                {
                    if seen_ids.insert(hit.id.to_string()) {
                        pinned.push(hit);
                    }
                }
            }
        }
        for class in SHARED_PINNED_CLASSES {
            for hit in self
                .search_class(shared_scope(agent), *class, PINNED_LIMIT, None, false)
                .await
            {
                if seen_ids.insert(hit.id.to_string()) {
                    pinned.push(hit);
                }
            }
        }

        if hits.is_empty() && pinned.is_empty() {
            info!(
                run_id = %ctx.run_id,
                query_len = query_text.len(),
                "memory-recall: skip (zero hits in scope, no pinned)",
            );
            return Decision::Continue;
        }

        let hint = build_hint(&pinned, &hits);
        let mut new_req = req.clone();
        if !prepend_text_into_last_user_msg(&mut new_req.messages, &hint) {
            return Decision::Continue;
        }
        info!(
            run_id = %ctx.run_id,
            hits = hits.len(),
            pinned = pinned.len(),
            "memory-recall: prefixed memory hits onto last user message",
        );
        Decision::Replace(new_req)
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        self.fired.lock().await.remove(&ctx.run_id);
    }
}

fn extract_last_user_text(messages: &[Value]) -> Option<String> {
    let last = messages.last()?;
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = last.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let arr = content.as_array()?;
    let mut buf = String::new();
    for block in arr {
        if block.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = block.get("text").and_then(Value::as_str)
        {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }
    if buf.is_empty() { None } else { Some(buf) }
}

/// Prefix `hint` onto the content of the trailing user message. Returns
/// false when the trailing message is not a recognisable user-text
/// shape and nothing was modified.
fn prepend_text_into_last_user_msg(messages: &mut [Value], hint: &str) -> bool {
    let Some(last) = messages.last_mut() else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(content) = last.get_mut("content") else {
        return false;
    };
    match content {
        Value::String(existing) => {
            *existing = format!("{hint}\n\n{existing}");
            true
        }
        Value::Array(blocks) => {
            blocks.insert(0, json!({"type": "text", "text": hint}));
            true
        }
        _ => false,
    }
}

fn build_hint(pinned: &[MemoryHit], hits: &[MemoryHit]) -> String {
    let mut out = String::new();
    out.push_str(
        "[Memory recall (auto-injected). The next [User question] block is the actual turn:]\n",
    );
    if !pinned.is_empty() {
        out.push_str("\n[Profile/Notes (always-pinned, ");
        out.push_str(&pinned.len().to_string());
        out.push_str(" rows):]\n");
        for hit in pinned {
            append_hit(&mut out, hit);
        }
    }
    if !hits.is_empty() {
        out.push_str("\n[Recall (similarity, top ");
        out.push_str(&hits.len().to_string());
        out.push_str("):]\n");
        for hit in hits {
            append_hit(&mut out, hit);
        }
    }
    out.push_str("\n\n[User question]:\n");
    out
}

fn append_hit(out: &mut String, hit: &MemoryHit) {
    let snip = truncate(&hit.body, BODY_CAP);
    let id_str = hit.id.to_string();
    let id_short = id_str.get(..8).unwrap_or(&id_str);
    out.push_str("\n- [");
    out.push_str(id_short);
    out.push_str("] ");
    out.push_str(&snip);
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::from(&s[..end]);
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_last_user_text_handles_string_content() {
        let messages = vec![
            json!({"role": "system", "content": "ignored"}),
            json!({"role": "user", "content": "hello world"}),
        ];
        assert_eq!(
            extract_last_user_text(&messages).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn extract_last_user_text_handles_array_content() {
        let messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "part a"},
                {"type": "text", "text": "part b"},
            ],
        })];
        assert_eq!(
            extract_last_user_text(&messages).as_deref(),
            Some("part a\npart b"),
        );
    }

    #[test]
    fn extract_last_user_text_skips_non_user_trailing() {
        let messages = vec![
            json!({"role": "user", "content": "earlier"}),
            json!({"role": "assistant", "content": "reply"}),
        ];
        assert_eq!(extract_last_user_text(&messages), None);
    }

    #[test]
    fn prepend_modifies_string_content() {
        let mut messages = vec![json!({"role": "user", "content": "question"})];
        assert!(prepend_text_into_last_user_msg(&mut messages, "HINT"));
        assert_eq!(
            messages[0].get("content").and_then(Value::as_str),
            Some("HINT\n\nquestion"),
        );
    }

    #[test]
    fn prepend_modifies_array_content() {
        let mut messages = vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "question"}],
        })];
        assert!(prepend_text_into_last_user_msg(&mut messages, "HINT"));
        let blocks = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .expect("array");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].get("text").and_then(Value::as_str), Some("HINT"),);
        assert_eq!(
            blocks[1].get("text").and_then(Value::as_str),
            Some("question"),
        );
    }

    #[test]
    fn prepend_skips_non_user_trailing() {
        let mut messages = vec![json!({"role": "assistant", "content": "reply"})];
        assert!(!prepend_text_into_last_user_msg(&mut messages, "HINT"));
    }

    #[test]
    fn truncate_keeps_short_strings_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_adds_ellipsis_on_overflow() {
        let s = "0123456789abcdef";
        let t = truncate(s, 8);
        assert!(t.ends_with("..."));
        assert!(t.starts_with("01234567"));
    }

    /// `owner_map` as `MemoryRecallHook::new` builds it: every owner string
    /// of a user maps to that user's full owner set.
    fn user_owner_map() -> HashMap<String, Vec<String>> {
        let owners = vec!["matrix:@t".to_owned(), "telegram:1".to_owned()];
        let mut map = HashMap::new();
        for owner in &owners {
            map.insert(owner.clone(), owners.clone());
        }
        map
    }

    #[test]
    fn recall_owners_relayed_run_impersonates_origin() {
        let subject = Subject::new("agent:assistant").with_attr(ORIGIN_OWNER_ATTR, "matrix:@t");
        let mut got = resolve_recall_owners(&user_owner_map(), &subject);
        got.sort();
        assert_eq!(got, vec!["matrix:@t".to_owned(), "telegram:1".to_owned()]);
    }

    #[test]
    fn recall_owners_normal_run_uses_subject_id() {
        // No origin attr: a plain channel run keyed on the human still
        // expands through the canonical-user map.
        let subject = Subject::new("telegram:1");
        let mut got = resolve_recall_owners(&user_owner_map(), &subject);
        got.sort();
        assert_eq!(got, vec!["matrix:@t".to_owned(), "telegram:1".to_owned()]);
    }

    #[test]
    fn recall_owners_unmapped_origin_returns_itself() {
        let subject = Subject::new("agent:assistant").with_attr(ORIGIN_OWNER_ATTR, "telegram:999");
        assert_eq!(
            resolve_recall_owners(&HashMap::new(), &subject),
            vec!["telegram:999".to_owned()],
        );
    }
}
