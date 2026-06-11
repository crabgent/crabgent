use chrono::{DateTime, Duration, TimeZone, Utc};
use crabgent_core::{MemoryScope, Owner};
use crabgent_store::{CronJob, CronJobId, CronSchedule, CronStore, InMemoryStore, Page, Store};
use crabgent_store_sqlite::SqliteStore;
use proptest::prelude::*;
use serde_json::json;

const OWNER_VALUES: &[&str] = &["alice", "bob", "carol"];
const CHANNEL_VALUES: &[&str] = &["C1", "C2", "C3"];
const CONV_VALUES: &[&str] = &["D1", "D2", "D3"];
const AGENT_VALUES: &[&str] = &["cron-a", "cron-b", "cron-c"];
const KIND_VALUES: &[&str] = &["public", "im", "broadcast"];
const MAX_FIXTURES: usize = 10;

#[derive(Clone, Copy, Debug, Default)]
struct ScopeSpec {
    owner: Option<&'static str>,
    channel: Option<&'static str>,
    conv: Option<&'static str>,
    agent: Option<&'static str>,
    kind: Option<&'static str>,
}

impl ScopeSpec {
    fn scope(self) -> MemoryScope {
        MemoryScope {
            owner: self.owner.map(Owner::new),
            channel: self.channel.map(str::to_owned),
            conv: self.conv.map(str::to_owned),
            agent: self.agent.map(str::to_owned),
            kind: self.kind.map(str::to_owned),
        }
    }
}

fn scope_value(pool: &'static [&'static str]) -> impl Strategy<Value = Option<&'static str>> {
    prop_oneof![Just(None), prop::sample::select(pool).prop_map(Some)]
}

fn scope_spec() -> impl Strategy<Value = ScopeSpec> {
    (
        scope_value(OWNER_VALUES),
        scope_value(CHANNEL_VALUES),
        scope_value(CONV_VALUES),
        scope_value(AGENT_VALUES),
        scope_value(KIND_VALUES),
    )
        .prop_map(|(owner, channel, conv, agent, kind)| ScopeSpec {
            owner,
            channel,
            conv,
            agent,
            kind,
        })
}

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("fixed timestamp is valid")
}

fn job(index: usize, scope: MemoryScope) -> CronJob {
    let now = base_time() + Duration::seconds(i64::try_from(index).unwrap_or(i64::MAX));
    CronJob {
        id: CronJobId::new(),
        name: format!("job-{index}"),
        scope,
        prompt: "say hi".into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: None,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: now,
        created_at: now,
        claimed_at: None,
    }
}

async fn list_ids<S>(store: &S, scope: &MemoryScope) -> Vec<CronJobId>
where
    S: Store,
    S::Cron: CronStore,
{
    store
        .cron()
        .list(scope, Page::first(MAX_FIXTURES))
        .await
        .expect("test result")
        .into_iter()
        .map(|job| job.id)
        .collect()
}

async fn compare_backends(
    fixtures: &[ScopeSpec],
    query: ScopeSpec,
) -> (Vec<CronJobId>, Vec<CronJobId>) {
    let memory = InMemoryStore::new();
    let sqlite = SqliteStore::open_in_memory()
        .await
        .expect("open sqlite store");
    for (index, spec) in fixtures.iter().copied().enumerate() {
        let job = job(index, spec.scope());
        memory.cron().create(&job).await.expect("test result");
        sqlite.cron().create(&job).await.expect("test result");
    }

    let scope = query.scope();
    (
        list_ids(&sqlite, &scope).await,
        list_ids(&memory, &scope).await,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn cron_scope_property_matches_in_memory(
        fixtures in prop::collection::vec(scope_spec(), 0..=MAX_FIXTURES),
        query in scope_spec(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");

        runtime.block_on(async move {
            let (sqlite_ids, memory_ids) = compare_backends(&fixtures, query).await;
            prop_assert_eq!(
                sqlite_ids,
                memory_ids,
                "query {:?}, fixtures {:?}",
                query,
                fixtures
            );
            Ok(())
        })?;
    }
}
