//! Web admin UI: memory browser, relation graph, and cron manager.
//!
//! Mounted on the same axum server as `mcp_http`. Static-key auth via
//! `HttpOnly` cookie scoped to `/admin`. The cookie value is the sha256
//! hex of the configured token; the raw token never re-enters request
//! state after `WebAdminState::new`. Constant-time comparison on every
//! request.
//!
//! Routes:
//!   GET    /admin                    HTML page (memory browser)
//!   GET    /admin/relations          HTML page (memory relation graph)
//!   GET    /admin/cron               HTML page (cron manager)
//!   GET    /admin/login              HTML form
//!   POST   /admin/login              form-urlencoded token, sets cookie
//!   GET    /admin/logout             clears cookie
//!   GET    /admin/api/memories       JSON list (scope + class + FTS filters)
//!   PUT    /admin/api/memories/:id   update body
//!   DELETE /admin/api/memories/:id   remove document
//!   GET    /admin/api/relations      JSON graph of memory relation edges
//!   GET    /admin/api/cron           JSON list of all cron jobs (scope filters)
//!   POST   /admin/api/cron           create cron job
//!   PUT    /admin/api/cron/:id       update cron job (partial)
//!   DELETE /admin/api/cron/:id       delete cron job

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, put},
};
use chrono::Utc;
use crabgent_core::{MemoryId, MemoryScope, Owner, SearchQuery};
use crabgent_cron::next_run;
use crabgent_log::{info, warn};
use crabgent_store::{
    CronJobId, CronStore, MemoryStore, Page, SessionId, SessionStore,
    records::{CronJob, CronJobUpdate, CronSchedule, MemoryDoc, ModelTargetDto},
};
use crabgent_store_sqlite::{SqliteCronStore, SqliteMemoryStore, SqliteSessionStore};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const COOKIE_NAME: &str = "crabgent_admin";
const COOKIE_MAX_AGE_S: u32 = 86_400;
const DEFAULT_LIMIT: u32 = 50;
const DEFAULT_GRAPH_LIMIT: u32 = 50;
const MAX_GRAPH_LIMIT: u32 = 100;
const DEFAULT_GRAPH_DEPTH: u32 = 2;
const MAX_GRAPH_DEPTH: u32 = 3;
const MAX_GRAPH_NODES: usize = 180;
const PAGE_HTML: &str = include_str!("../templates/memory.html");
const RELATIONS_HTML: &str = include_str!("../templates/relations.html");
const CRON_HTML: &str = include_str!("../templates/cron.html");
const SESSIONS_HTML: &str = include_str!("../templates/sessions.html");
const LOGIN_HTML: &str = include_str!("../templates/login.html");

#[derive(Clone)]
pub struct WebAdminState {
    memory: SqliteMemoryStore,
    cron: SqliteCronStore,
    session: SqliteSessionStore,
    /// Names of agents configured in this host; surfaced to the UI as
    /// dropdown options.
    agents: Arc<Vec<String>>,
    /// `sha256(auth_token)` hex. Compared in constant time against
    /// cookie + submitted-form values.
    expected_hash: Arc<String>,
}

impl WebAdminState {
    #[must_use]
    pub fn new(
        memory: SqliteMemoryStore,
        cron: SqliteCronStore,
        session: SqliteSessionStore,
        agents: Vec<String>,
        auth_token: &SecretString,
    ) -> Self {
        let hash = sha256_hex(auth_token.expose_secret().as_bytes());
        Self {
            memory,
            cron,
            session,
            agents: Arc::new(agents),
            expected_hash: Arc::new(hash),
        }
    }
}

pub fn build_router(state: WebAdminState) -> Router {
    Router::new()
        .route("/admin", get(page))
        .route("/admin/relations", get(relations_page))
        .route("/admin/cron", get(cron_page))
        .route("/admin/sessions", get(sessions_page))
        .route("/admin/login", get(login_page).post(login_submit))
        .route("/admin/logout", get(logout))
        .route("/admin/api/facets", get(facets))
        .route("/admin/api/memories", get(list))
        .route("/admin/api/memories/{id}", put(update).delete(remove))
        .route("/admin/api/relations", get(relations_graph))
        .route("/admin/api/cron", get(cron_list).post(cron_create))
        .route("/admin/api/cron/{id}", put(cron_update).delete(cron_delete))
        .route("/admin/api/sessions", get(sessions_list))
        .route("/admin/api/sessions/{id}", get(sessions_get))
        .with_state(state)
}

async fn page(State(state): State<WebAdminState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return Redirect::to("/admin/login").into_response();
    }
    Html(PAGE_HTML).into_response()
}

async fn cron_page(State(state): State<WebAdminState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return Redirect::to("/admin/login").into_response();
    }
    Html(CRON_HTML).into_response()
}

async fn relations_page(State(state): State<WebAdminState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return Redirect::to("/admin/login").into_response();
    }
    Html(RELATIONS_HTML).into_response()
}

async fn sessions_page(State(state): State<WebAdminState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return Redirect::to("/admin/login").into_response();
    }
    Html(SESSIONS_HTML).into_response()
}

#[derive(Serialize)]
struct FacetsResponse {
    agents: Vec<String>,
    channels: Vec<&'static str>,
    kinds: Vec<&'static str>,
    classes: Vec<&'static str>,
}

async fn facets(State(state): State<WebAdminState>, headers: HeaderMap) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let resp = FacetsResponse {
        agents: state.agents.as_ref().clone(),
        channels: vec!["matrix", "telegram"],
        kinds: vec!["direct", "group"],
        classes: vec![
            "semantic",
            "episodic",
            "recency",
            "user_profile",
            "notes",
            "skill",
            "tools",
        ],
    };
    Json(resp).into_response()
}

async fn login_page(State(state): State<WebAdminState>, headers: HeaderMap) -> Response {
    if is_authed(&state, &headers) {
        return Redirect::to("/admin").into_response();
    }
    Html(LOGIN_HTML).into_response()
}

#[derive(Deserialize)]
struct LoginForm {
    token: String,
}

async fn login_submit(State(state): State<WebAdminState>, Form(form): Form<LoginForm>) -> Response {
    let submitted_hash = sha256_hex(form.token.as_bytes());
    if constant_eq(submitted_hash.as_bytes(), state.expected_hash.as_bytes()) {
        info!("admin: login successful");
        let cookie = format!(
            "{COOKIE_NAME}={value}; HttpOnly; SameSite=Strict; Path=/admin; Max-Age={COOKIE_MAX_AGE_S}",
            value = state.expected_hash.as_str(),
        );
        let mut headers = HeaderMap::new();
        if let Ok(hv) = HeaderValue::from_str(&cookie) {
            headers.insert(header::SET_COOKIE, hv);
        }
        headers.insert(header::LOCATION, HeaderValue::from_static("/admin"));
        (StatusCode::SEE_OTHER, headers).into_response()
    } else {
        warn!("admin: login failed (token mismatch)");
        let html = LOGIN_HTML.replace(
            "<!-- ERROR_SLOT -->",
            "<div class=\"err\">invalid token</div>",
        );
        (StatusCode::UNAUTHORIZED, Html(html)).into_response()
    }
}

async fn logout() -> Response {
    let cookie = format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=0");
    let mut headers = HeaderMap::new();
    if let Ok(hv) = HeaderValue::from_str(&cookie) {
        headers.insert(header::SET_COOKIE, hv);
    }
    headers.insert(header::LOCATION, HeaderValue::from_static("/admin/login"));
    (StatusCode::SEE_OTHER, headers).into_response()
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    conv: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
    #[serde(default)]
    include_archived: Option<bool>,
    #[serde(default)]
    include_expired: Option<bool>,
}

#[derive(Serialize)]
struct ListResponse {
    memories: Vec<crabgent_store::records::MemoryDoc>,
}

async fn list(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Query(qp): Query<ListQuery>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let scope = build_scope(&qp);
    let mut query = SearchQuery::new(qp.q.unwrap_or_default()).scope(scope);
    if let Some(class) = qp.class.filter(|s| !s.is_empty()) {
        query = query.class(class);
    }
    if qp.include_archived.unwrap_or(false) {
        query = query.include_archived();
    }
    if qp.include_expired.unwrap_or(false) {
        query = query.include_expired();
    }
    query = query.limit(qp.limit.unwrap_or(DEFAULT_LIMIT));
    query = query.offset(qp.offset.unwrap_or(0));
    let hits = match state.memory.search(&query).await {
        Ok(hits) => hits,
        Err(err) => {
            warn!(error = %err, "admin: memory search failed");
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    let mut docs = Vec::with_capacity(hits.len());
    for hit in hits {
        match state.memory.get(&hit.id).await {
            Ok(Some(doc)) => docs.push(doc),
            Ok(None) => {}
            Err(err) => warn!(
                id = %hit.id,
                error = %err,
                "admin: memory get failed during list; skipping",
            ),
        }
    }
    Json(ListResponse { memories: docs }).into_response()
}

#[derive(Deserialize)]
struct UpdateBody {
    body: String,
}

async fn update(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<UpdateBody>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let Ok(mid) = id.parse::<MemoryId>() else {
        return error_json(StatusCode::BAD_REQUEST, "invalid memory id".to_owned());
    };
    match state.memory.update_body(&mid, payload.body).await {
        Ok(true) => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(false) => error_json(StatusCode::NOT_FOUND, "not found".to_owned()),
        Err(err) => {
            warn!(id = %mid, error = %err, "admin: update_body failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

async fn remove(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let Ok(mid) = id.parse::<MemoryId>() else {
        return error_json(StatusCode::BAD_REQUEST, "invalid memory id".to_owned());
    };
    match state.memory.delete(&mid).await {
        Ok(true) => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(false) => error_json(StatusCode::NOT_FOUND, "not found".to_owned()),
        Err(err) => {
            warn!(id = %mid, error = %err, "admin: delete failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

#[derive(Deserialize, Default)]
struct RelationGraphQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    relation_type: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    conv: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    connected_only: Option<bool>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    depth: Option<u32>,
}

#[derive(Serialize)]
struct RelationGraphResponse {
    roots: Vec<String>,
    nodes: Vec<RelationGraphNode>,
    edges: Vec<RelationGraphEdge>,
    truncated: bool,
}

#[derive(Serialize)]
struct RelationGraphNode {
    id: String,
    short_id: String,
    body: String,
    excerpt: String,
    class: Option<String>,
    scope: MemoryScope,
    importance: Option<f32>,
    created_at: String,
    updated_at: String,
    archived: bool,
    has_embedding: bool,
    degree: usize,
}

#[derive(Serialize)]
struct RelationGraphEdge {
    id: String,
    from_id: String,
    to_id: String,
    relation_type: String,
    scope: MemoryScope,
    created_at: String,
    depth: u32,
}

async fn relations_graph(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Query(qp): Query<RelationGraphQuery>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    match build_relation_graph(&state.memory, qp).await {
        Ok(graph) => Json(graph).into_response(),
        Err(GraphError::BadRequest(message)) => error_json(StatusCode::BAD_REQUEST, message),
        Err(GraphError::Store(err)) => {
            warn!(error = %err, "admin: relation graph failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

enum GraphError {
    BadRequest(String),
    Store(crabgent_store::StoreError),
}

impl From<crabgent_store::StoreError> for GraphError {
    fn from(value: crabgent_store::StoreError) -> Self {
        Self::Store(value)
    }
}

async fn build_relation_graph(
    memory: &SqliteMemoryStore,
    qp: RelationGraphQuery,
) -> Result<RelationGraphResponse, GraphError> {
    let scope = build_graph_scope(&qp);
    let limit = qp
        .limit
        .unwrap_or(DEFAULT_GRAPH_LIMIT)
        .clamp(1, MAX_GRAPH_LIMIT);
    let depth = qp
        .depth
        .unwrap_or(DEFAULT_GRAPH_DEPTH)
        .clamp(1, MAX_GRAPH_DEPTH);
    let relation_type = qp.relation_type.clone().filter(|s| !s.is_empty());
    let roots = graph_roots(memory, &scope, &qp, limit).await?;
    if roots.is_empty() {
        return Ok(RelationGraphResponse {
            roots: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        });
    }
    let mut walk = walk_relations(memory, &scope, roots, relation_type.as_deref(), depth).await?;
    if qp.connected_only.unwrap_or(false) {
        walk.retain_connected();
    }
    let nodes = load_graph_nodes(memory, &walk.node_order, &walk.edges).await?;
    Ok(RelationGraphResponse {
        roots: walk.roots.iter().map(MemoryId::to_string).collect(),
        nodes,
        edges: walk.edges,
        truncated: walk.truncated,
    })
}

async fn graph_roots(
    memory: &SqliteMemoryStore,
    scope: &MemoryScope,
    qp: &RelationGraphQuery,
    limit: u32,
) -> Result<Vec<MemoryId>, GraphError> {
    if let Some(root) = qp.root.as_deref().filter(|s| !s.is_empty()) {
        return MemoryId::from_str(root)
            .map(|id| vec![id])
            .map_err(|err| GraphError::BadRequest(format!("root: {err}")));
    }
    let mut query = SearchQuery::new(qp.q.clone().unwrap_or_default()).scope(scope.clone());
    if let Some(class) = qp.class.as_deref().filter(|s| !s.is_empty()) {
        query = query.class(class);
    }
    query = query.limit(limit);
    let hits = memory.search(&query).await?;
    Ok(hits.into_iter().map(|hit| hit.id).collect())
}

struct RelationWalk {
    roots: Vec<MemoryId>,
    node_order: Vec<MemoryId>,
    visited: HashSet<MemoryId>,
    edge_keys: HashSet<String>,
    edges: Vec<RelationGraphEdge>,
    truncated: bool,
}

async fn walk_relations(
    memory: &SqliteMemoryStore,
    scope: &MemoryScope,
    roots: Vec<MemoryId>,
    relation_type: Option<&str>,
    depth: u32,
) -> Result<RelationWalk, GraphError> {
    let mut walk = RelationWalk::new(roots);
    let mut frontier = walk.node_order.clone();
    for current_depth in 1..=depth {
        if frontier.is_empty() || walk.truncated {
            break;
        }
        let neighbors = memory.relation_neighbors(&frontier, scope).await?;
        let mut next = Vec::new();
        for relation in neighbors {
            if relation_type.is_some_and(|wanted| relation.relation_type.as_str() != wanted) {
                continue;
            }
            walk.record_edge(&relation, current_depth);
            walk.add_node(relation.from_id, &mut next);
            walk.add_node(relation.to_id, &mut next);
        }
        frontier = next;
    }
    Ok(walk)
}

impl RelationWalk {
    fn new(roots: Vec<MemoryId>) -> Self {
        let mut visited = HashSet::new();
        let mut node_order = Vec::new();
        for root in &roots {
            if visited.insert(root.clone()) {
                node_order.push(root.clone());
            }
        }
        Self {
            roots,
            node_order,
            visited,
            edge_keys: HashSet::new(),
            edges: Vec::new(),
            truncated: false,
        }
    }

    fn add_node(&mut self, id: MemoryId, next: &mut Vec<MemoryId>) {
        if self.truncated || self.visited.contains(&id) {
            return;
        }
        if self.visited.len() >= MAX_GRAPH_NODES {
            self.truncated = true;
            return;
        }
        self.visited.insert(id.clone());
        self.node_order.push(id.clone());
        next.push(id);
    }

    fn record_edge(&mut self, relation: &crabgent_store::MemoryRelation, depth: u32) {
        let key = relation.id.to_string();
        if !self.edge_keys.insert(key) {
            return;
        }
        self.edges.push(RelationGraphEdge {
            id: relation.id.to_string(),
            from_id: relation.from_id.to_string(),
            to_id: relation.to_id.to_string(),
            relation_type: relation.relation_type.as_str().to_owned(),
            scope: relation.scope.clone(),
            created_at: relation.created_at.to_rfc3339(),
            depth,
        });
    }

    fn retain_connected(&mut self) {
        let connected = connected_node_ids(&self.edges);
        if connected.is_empty() {
            return;
        }
        self.node_order.retain(|id| connected.contains(id));
        self.roots.retain(|id| connected.contains(id));
        self.visited = self.node_order.iter().cloned().collect();
    }
}

fn connected_node_ids(edges: &[RelationGraphEdge]) -> HashSet<MemoryId> {
    let mut ids = HashSet::new();
    for edge in edges {
        if let Ok(id) = MemoryId::from_str(&edge.from_id) {
            ids.insert(id);
        }
        if let Ok(id) = MemoryId::from_str(&edge.to_id) {
            ids.insert(id);
        }
    }
    ids
}

async fn load_graph_nodes(
    memory: &SqliteMemoryStore,
    node_ids: &[MemoryId],
    edges: &[RelationGraphEdge],
) -> Result<Vec<RelationGraphNode>, GraphError> {
    let degree = graph_degrees(edges);
    let mut nodes = Vec::with_capacity(node_ids.len());
    for id in node_ids {
        if let Some(doc) = memory.get(id).await? {
            nodes.push(node_from_doc(doc, degree.get(&id.to_string()).copied()));
        } else {
            warn!(id = %id, "admin: relation graph node missing");
        }
    }
    Ok(nodes)
}

fn graph_degrees(edges: &[RelationGraphEdge]) -> HashMap<String, usize> {
    let mut degree = HashMap::new();
    for edge in edges {
        *degree.entry(edge.from_id.clone()).or_insert(0) += 1;
        *degree.entry(edge.to_id.clone()).or_insert(0) += 1;
    }
    degree
}

fn node_from_doc(doc: MemoryDoc, degree: Option<usize>) -> RelationGraphNode {
    let body = doc.body;
    let excerpt = graph_excerpt(&body, 180);
    let has_embedding = doc.embedding.is_some();
    let id = doc.id.to_string();
    let short_id = id.chars().take(8).collect();
    RelationGraphNode {
        short_id,
        id,
        body,
        excerpt,
        class: doc.class,
        scope: doc.scope,
        importance: doc.importance,
        created_at: doc.created_at.to_rfc3339(),
        updated_at: doc.updated_at.to_rfc3339(),
        archived: doc.archived_at.is_some(),
        has_embedding,
        degree: degree.unwrap_or(0),
    }
}

fn graph_excerpt(body: &str, max_chars: usize) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut out = compact
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

#[derive(Deserialize, Default)]
struct CronListQuery {
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    conv: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

#[derive(Serialize)]
struct CronListResponse {
    jobs: Vec<CronJob>,
}

async fn cron_list(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Query(qp): Query<CronListQuery>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let scope = MemoryScope {
        owner: qp
            .owner
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(Owner::new),
        channel: qp.channel.filter(|s| !s.is_empty()),
        conv: qp.conv.filter(|s| !s.is_empty()),
        agent: qp.agent.filter(|s| !s.is_empty()),
        kind: qp.kind.filter(|s| !s.is_empty()),
    };
    let limit = qp.limit.unwrap_or(DEFAULT_LIMIT) as usize;
    let offset = qp.offset.unwrap_or(0) as usize;
    let page = Page { limit, offset };
    match state.cron.list(&scope, page).await {
        Ok(jobs) => Json(CronListResponse { jobs }).into_response(),
        Err(err) => {
            warn!(error = %err, "admin: cron list failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

#[derive(Deserialize)]
struct CronCreateBody {
    name: String,
    prompt: String,
    schedule: CronSchedule,
    #[serde(default)]
    scope: MemoryScope,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    run_once: Option<bool>,
    #[serde(default)]
    model_override: Option<ModelTargetDto>,
    #[serde(default)]
    pre_command: Option<String>,
    #[serde(default = "Value::default")]
    delivery_ctx: Value,
}

async fn cron_create(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Json(body): Json<CronCreateBody>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    if let Err(msg) = validate_schedule(&body.schedule) {
        return error_json(StatusCode::BAD_REQUEST, msg);
    }
    let now = Utc::now();
    let Some(next) = next_run(&body.schedule, now) else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "schedule has no valid next run".to_owned(),
        );
    };
    let job = CronJob {
        id: CronJobId::new(),
        name: body.name,
        scope: body.scope,
        prompt: body.prompt,
        schedule: body.schedule,
        enabled: body.enabled.unwrap_or(true),
        run_once: body.run_once.unwrap_or(false),
        model_override: body.model_override,
        reasoning_effort_override: None,
        pre_command: body.pre_command,
        delivery_ctx: body.delivery_ctx,
        last_run: None,
        next_run: next,
        created_at: now,
        claimed_at: None,
    };
    match state.cron.create(&job).await {
        Ok(()) => {
            info!(id = %job.id, name = %job.name, "admin: cron created");
            (StatusCode::CREATED, Json(serde_json::json!({"job": job}))).into_response()
        }
        Err(err) => {
            warn!(error = %err, "admin: cron create failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

#[derive(Deserialize, Default)]
struct CronUpdateBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    schedule: Option<CronSchedule>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    run_once: Option<bool>,
    #[serde(default)]
    #[allow(clippy::option_option)] // tri-state PATCH: absent vs explicit-null vs set
    model_override: Option<Option<ModelTargetDto>>,
    #[serde(default)]
    #[allow(clippy::option_option)] // tri-state PATCH: absent vs explicit-null vs set
    pre_command: Option<Option<String>>,
    #[serde(default)]
    delivery_ctx: Option<Value>,
}

async fn cron_update(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CronUpdateBody>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let Ok(job_id) = CronJobId::from_str(&id) else {
        return error_json(StatusCode::BAD_REQUEST, "invalid cron id".to_owned());
    };
    if let Some(ref s) = body.schedule
        && let Err(msg) = validate_schedule(s)
    {
        return error_json(StatusCode::BAD_REQUEST, msg);
    }
    let next_run_override = body.schedule.as_ref().and_then(|s| next_run(s, Utc::now()));
    let update = CronJobUpdate {
        name: body.name,
        prompt: body.prompt,
        schedule: body.schedule,
        enabled: body.enabled,
        run_once: body.run_once,
        model_override: body.model_override,
        reasoning_effort_override: None,
        pre_command: body.pre_command,
        delivery_ctx: body.delivery_ctx,
        next_run: next_run_override,
    };
    match state.cron.update(&job_id, &update).await {
        Ok(true) => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(false) => error_json(StatusCode::NOT_FOUND, "not found".to_owned()),
        Err(err) => {
            warn!(id = %job_id, error = %err, "admin: cron update failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

async fn cron_delete(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let Ok(job_id) = CronJobId::from_str(&id) else {
        return error_json(StatusCode::BAD_REQUEST, "invalid cron id".to_owned());
    };
    match state.cron.delete(&job_id).await {
        Ok(true) => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(false) => error_json(StatusCode::NOT_FOUND, "not found".to_owned()),
        Err(err) => {
            warn!(id = %job_id, error = %err, "admin: cron delete failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

#[derive(Deserialize, Default)]
struct SessionListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    conv: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

#[derive(Serialize)]
struct SessionSummary {
    id: String,
    owner: String,
    thread: Option<String>,
    title: Option<String>,
    channel: Option<String>,
    conv: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
    message_count: usize,
    has_summary: bool,
    has_compaction_summary: bool,
    created_at: String,
    updated_at: String,
    excerpt: Option<String>,
}

#[derive(Serialize)]
struct SessionsResponse {
    sessions: Vec<SessionSummary>,
}

async fn sessions_list(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Query(qp): Query<SessionListQuery>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let scope = MemoryScope {
        owner: qp
            .owner
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(crabgent_core::Owner::new),
        channel: qp.channel.filter(|s| !s.is_empty()),
        conv: qp.conv.filter(|s| !s.is_empty()),
        agent: qp.agent.filter(|s| !s.is_empty()),
        kind: qp.kind.filter(|s| !s.is_empty()),
    };
    let mut query = crabgent_core::SearchQuery::new(qp.q.unwrap_or_default()).scope(scope);
    query = query.limit(qp.limit.unwrap_or(DEFAULT_LIMIT));
    query = query.offset(qp.offset.unwrap_or(0));
    let hits = match state.session.search(&query).await {
        Ok(hits) => hits,
        Err(err) => {
            warn!(error = %err, "admin: sessions search failed");
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    let mut sessions = Vec::with_capacity(hits.len());
    for hit in hits {
        match state.session.load(&hit.session_id).await {
            Ok(Some(s)) => sessions.push(SessionSummary {
                id: s.id.to_string(),
                owner: s.owner.to_string(),
                thread: s.thread.map(|t| t.to_string()),
                title: s.title.clone(),
                channel: s.scope.channel.clone(),
                conv: s.scope.conv.clone(),
                agent: s.scope.agent.clone(),
                kind: s.scope.kind.clone(),
                message_count: s.messages.len(),
                has_summary: s.summary.is_some(),
                has_compaction_summary: s.compaction_summary.is_some(),
                created_at: s.created_at.to_rfc3339(),
                updated_at: s.updated_at.to_rfc3339(),
                excerpt: Some(hit.excerpt),
            }),
            Ok(None) => {}
            Err(err) => warn!(id = %hit.session_id, error = %err, "admin: session load failed"),
        }
    }
    Json(SessionsResponse { sessions }).into_response()
}

async fn sessions_get(
    State(state): State<WebAdminState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !is_authed(&state, &headers) {
        return unauthorized_json();
    }
    let Ok(sid) = id.parse::<SessionId>() else {
        return error_json(StatusCode::BAD_REQUEST, "invalid session id".to_owned());
    };
    match state.session.load(&sid).await {
        Ok(Some(s)) => Json(serde_json::json!({"session": s})).into_response(),
        Ok(None) => error_json(StatusCode::NOT_FOUND, "not found".to_owned()),
        Err(err) => {
            warn!(id = %sid, error = %err, "admin: session load failed");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

fn validate_schedule(s: &CronSchedule) -> Result<(), String> {
    match (s.interval_secs, s.cron_expr.as_deref()) {
        (None, None) => Err("schedule needs interval_secs or cron_expr".to_owned()),
        (Some(_), Some(_)) => {
            Err("schedule cannot have both interval_secs and cron_expr".to_owned())
        }
        (Some(0), _) => Err("interval_secs must be > 0".to_owned()),
        _ => Ok(()),
    }
}

fn build_scope(qp: &ListQuery) -> MemoryScope {
    scope_from_fields(
        qp.owner.as_deref(),
        qp.channel.as_deref(),
        qp.conv.as_deref(),
        qp.agent.as_deref(),
        qp.kind.as_deref(),
    )
}

fn build_graph_scope(qp: &RelationGraphQuery) -> MemoryScope {
    scope_from_fields(
        qp.owner.as_deref(),
        qp.channel.as_deref(),
        qp.conv.as_deref(),
        qp.agent.as_deref(),
        qp.kind.as_deref(),
    )
}

fn scope_from_fields(
    owner: Option<&str>,
    channel: Option<&str>,
    conv: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
) -> MemoryScope {
    let mut scope = MemoryScope::global();
    if let Some(o) = owner.filter(|s| !s.is_empty()) {
        scope.owner = Some(Owner::new(o));
    }
    if let Some(c) = channel.filter(|s| !s.is_empty()) {
        scope.channel = Some(c.to_owned());
    }
    if let Some(c) = conv.filter(|s| !s.is_empty()) {
        scope.conv = Some(c.to_owned());
    }
    if let Some(a) = agent.filter(|s| !s.is_empty()) {
        scope.agent = Some(a.to_owned());
    }
    if let Some(k) = kind.filter(|s| !s.is_empty()) {
        scope.kind = Some(k.to_owned());
    }
    scope
}

fn is_authed(state: &WebAdminState, headers: &HeaderMap) -> bool {
    let Some(cookie_header) = headers.get(header::COOKIE) else {
        return false;
    };
    let Ok(value) = cookie_header.to_str() else {
        return false;
    };
    for pair in value.split(';') {
        let pair = pair.trim();
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k.trim() == COOKIE_NAME {
            return constant_eq(v.trim().as_bytes(), state.expected_hash.as_bytes());
        }
    }
    false
}

fn unauthorized_json() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "unauthorized"})),
    )
        .into_response()
}

#[allow(clippy::needless_pass_by_value)] // message is moved into the JSON error body
fn error_json(status: StatusCode, message: String) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_64_chars() {
        let hex = sha256_hex(b"abc");
        assert_eq!(hex.len(), 64);
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn constant_eq_matches_byte_for_byte() {
        assert!(constant_eq(b"alpha", b"alpha"));
        assert!(!constant_eq(b"alpha", b"alpb"));
        assert!(!constant_eq(b"alpha", b"alphb"));
    }

    fn state_with_token(token: &str) -> WebAdminState {
        let (memory, cron, session) = futures_executor_block_on_in_memory();
        let secret = SecretString::from(token.to_owned());
        WebAdminState::new(memory, cron, session, vec![], &secret)
    }

    fn futures_executor_block_on_in_memory()
    -> (SqliteMemoryStore, SqliteCronStore, SqliteSessionStore) {
        use crabgent_store::Store;
        let store = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
            .block_on(crabgent_store_sqlite::SqliteStore::open_in_memory())
            .expect("open in-memory sqlite");
        (
            store.memory().clone(),
            store.cron().clone(),
            store.session().clone(),
        )
    }

    fn header_with_cookie(name: &str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{name}={value}")).expect("valid cookie"),
        );
        h
    }

    #[test]
    fn is_authed_rejects_missing_cookie() {
        let state = state_with_token("hunter2");
        assert!(!is_authed(&state, &HeaderMap::new()));
    }

    #[test]
    fn is_authed_rejects_wrong_cookie_value() {
        let state = state_with_token("hunter2");
        let bad = header_with_cookie(COOKIE_NAME, "deadbeef");
        assert!(!is_authed(&state, &bad));
    }

    #[test]
    fn is_authed_accepts_matching_cookie_hash() {
        let state = state_with_token("hunter2");
        let good = header_with_cookie(COOKIE_NAME, &sha256_hex(b"hunter2"));
        assert!(is_authed(&state, &good));
    }

    #[test]
    fn is_authed_ignores_other_cookies_in_jar() {
        let state = state_with_token("hunter2");
        let mut h = HeaderMap::new();
        let jar = format!(
            "session=abc; {}={}; theme=dark",
            COOKIE_NAME,
            sha256_hex(b"hunter2")
        );
        h.insert(header::COOKIE, HeaderValue::from_str(&jar).expect("ok"));
        assert!(is_authed(&state, &h));
    }
}
