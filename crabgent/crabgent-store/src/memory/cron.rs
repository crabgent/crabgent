//! In-memory [`CronStore`] backed by a `HashMap` behind a `Mutex`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use crabgent_core::MemoryScope;

use crate::error::StoreError;
use crate::ids::CronJobId;
use crate::page::Page;
use crate::records::{CronJob, CronJobUpdate};
use crate::scope_query::ScopeQuery;
use crate::traits::CronStore;

#[derive(Default)]
pub struct MemoryCronStore {
    inner: Mutex<HashMap<CronJobId, CronJob>>,
}

impl MemoryCronStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<CronJobId, CronJob>>, StoreError> {
        self.inner
            .lock()
            .map_err(|e| StoreError::backend(format!("cron mutex poisoned: {e}")))
    }
}

#[async_trait]
impl CronStore for MemoryCronStore {
    async fn create(&self, job: &CronJob) -> Result<(), StoreError> {
        let mut jobs = self.lock()?;
        if jobs.contains_key(&job.id) {
            return Err(StoreError::Conflict(format!(
                "cron job already exists: {}",
                job.id
            )));
        }
        jobs.insert(job.id.clone(), job.clone());
        Ok(())
    }

    async fn get(&self, id: &CronJobId) -> Result<Option<CronJob>, StoreError> {
        let jobs = self.lock()?;
        Ok(jobs.get(id).cloned())
    }

    async fn list(&self, scope: &MemoryScope, page: Page) -> Result<Vec<CronJob>, StoreError> {
        let jobs = self.lock()?;
        let scope_query = ScopeQuery::filter(scope);
        let mut filtered: Vec<&CronJob> = jobs
            .values()
            .filter(|job| scope_query.matches(&job.scope))
            .collect();
        filtered.sort_by_key(|a| a.created_at);
        Ok(filtered
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .cloned()
            .collect())
    }

    async fn update(&self, id: &CronJobId, update: &CronJobUpdate) -> Result<bool, StoreError> {
        let mut jobs = self.lock()?;
        if let Some(job) = jobs.get_mut(id) {
            update.apply_to(job);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn delete(&self, id: &CronJobId) -> Result<bool, StoreError> {
        let mut jobs = self.lock()?;
        Ok(jobs.remove(id).is_some())
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<CronJob>, StoreError> {
        let mut jobs = self.lock()?;
        let mut due_ids: Vec<CronJobId> = jobs
            .values()
            .filter(|j| j.enabled && j.claimed_at.is_none() && j.next_run <= now)
            .map(|j| (j.id.clone(), j.next_run))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        due_ids.sort_by_key(|id| jobs.get(id).map(|j| j.next_run));
        due_ids.truncate(limit);
        let mut claimed: Vec<CronJob> = Vec::with_capacity(due_ids.len());
        for id in &due_ids {
            if let Some(job) = jobs.get_mut(id) {
                job.claimed_at = Some(now);
                claimed.push(job.clone());
            }
        }
        Ok(claimed)
    }

    async fn finish_claim(
        &self,
        id: &CronJobId,
        last_run: DateTime<Utc>,
        next_run: DateTime<Utc>,
        disable_run_once: bool,
    ) -> Result<(), StoreError> {
        let mut jobs = self.lock()?;
        let Some(job) = jobs.get_mut(id) else {
            return Ok(());
        };
        job.last_run = Some(last_run);
        job.next_run = next_run;
        job.claimed_at = None;
        if disable_run_once && job.run_once {
            job.enabled = false;
        }
        Ok(())
    }

    async fn release_claim_only(&self, id: &CronJobId) -> Result<(), StoreError> {
        let mut jobs = self.lock()?;
        let Some(job) = jobs.get_mut(id) else {
            return Ok(());
        };
        job.claimed_at = None;
        Ok(())
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<CronJobId>, StoreError> {
        let cutoff = Utc::now() - Duration::seconds(timeout_secs);
        let mut jobs = self.lock()?;
        let stuck: Vec<CronJobId> = jobs
            .values()
            .filter(|j| j.claimed_at.is_some_and(|c| c < cutoff))
            .map(|j| j.id.clone())
            .collect();
        for id in &stuck {
            if let Some(job) = jobs.get_mut(id) {
                job.claimed_at = None;
            }
        }
        Ok(stuck)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::CronSchedule;
    use chrono::TimeZone;
    use crabgent_core::Owner;
    use serde_json::json;

    fn make_job(now: DateTime<Utc>, scope: MemoryScope) -> CronJob {
        CronJob {
            id: CronJobId::new(),
            name: "ping".into(),
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

    fn owner_scope(owner: &str) -> MemoryScope {
        MemoryScope::for_owner(Owner::new(owner))
    }

    #[tokio::test]
    async fn create_and_get() {
        let store = MemoryCronStore::default();
        let job = make_job(Utc::now(), owner_scope("u"));
        store.create(&job).await.expect("test result");
        let got = store
            .get(&job.id)
            .await
            .expect("test result")
            .expect("test result");
        assert_eq!(got.name, "ping");
    }

    #[tokio::test]
    async fn duplicate_create_conflicts() {
        let store = MemoryCronStore::default();
        let job = make_job(Utc::now(), MemoryScope::global());
        store.create(&job).await.expect("test result");
        let err = store.create(&job).await.expect_err("expected error");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn delete_returns_true_then_false() {
        let store = MemoryCronStore::default();
        let job = make_job(Utc::now(), MemoryScope::global());
        store.create(&job).await.expect("test result");
        assert!(store.delete(&job.id).await.expect("test result"));
        assert!(!store.delete(&job.id).await.expect("test result"));
    }

    #[tokio::test]
    async fn cron_list_scope_owner_filter() {
        let store = MemoryCronStore::default();
        let alice_job = make_job(Utc::now(), owner_scope("alice"));
        let bob_job = make_job(Utc::now() + Duration::seconds(1), owner_scope("bob"));
        store.create(&alice_job).await.expect("test result");
        store.create(&bob_job).await.expect("test result");
        let alice_only = store
            .list(&owner_scope("alice"), Page::first(10))
            .await
            .expect("test result");
        assert_eq!(alice_only.len(), 1);
        assert_eq!(alice_only[0].id, alice_job.id);
    }

    #[tokio::test]
    async fn cron_list_scope_kind_filter() {
        let store = MemoryCronStore::default();
        let public_job = make_job(Utc::now(), MemoryScope::global().with_kind("public"));
        let im_job = make_job(
            Utc::now() + Duration::seconds(1),
            MemoryScope::global().with_kind("im"),
        );
        store.create(&public_job).await.expect("test result");
        store.create(&im_job).await.expect("test result");
        let public_only = store
            .list(&MemoryScope::global().with_kind("public"), Page::first(10))
            .await
            .expect("test result");
        assert_eq!(public_only.len(), 1);
        assert_eq!(public_only[0].id, public_job.id);
    }

    #[tokio::test]
    async fn cron_list_scope_multi_field_and() {
        let store = MemoryCronStore::default();
        let alice_im = make_job(Utc::now(), owner_scope("alice").with_kind("im"));
        let alice_public = make_job(
            Utc::now() + Duration::seconds(1),
            owner_scope("alice").with_kind("public"),
        );
        let bob_im = make_job(
            Utc::now() + Duration::seconds(2),
            owner_scope("bob").with_kind("im"),
        );
        store.create(&alice_im).await.expect("test result");
        store.create(&alice_public).await.expect("test result");
        store.create(&bob_im).await.expect("test result");
        let alice_im_only = store
            .list(&owner_scope("alice").with_kind("im"), Page::first(10))
            .await
            .expect("test result");
        assert_eq!(alice_im_only.len(), 1);
        assert_eq!(alice_im_only[0].id, alice_im.id);
    }

    #[tokio::test]
    async fn cron_list_scope_empty_returns_all() {
        let store = MemoryCronStore::default();
        let alice_job = make_job(Utc::now(), owner_scope("alice"));
        let global_job = make_job(Utc::now() + Duration::seconds(1), MemoryScope::global());
        store.create(&alice_job).await.expect("test result");
        store.create(&global_job).await.expect("test result");
        let all_jobs = store
            .list(&MemoryScope::global(), Page::first(10))
            .await
            .expect("test result");
        assert_eq!(all_jobs.len(), 2);
        assert_eq!(all_jobs[0].id, alice_job.id);
        assert_eq!(all_jobs[1].id, global_job.id);
    }

    #[tokio::test]
    async fn cron_list_scope_null_field_strict_match() {
        let store = MemoryCronStore::default();
        let channel_job = make_job(Utc::now(), MemoryScope::global().with_channel("C1"));
        let no_channel_job = make_job(Utc::now() + Duration::seconds(1), MemoryScope::global());
        store.create(&channel_job).await.expect("test result");
        store.create(&no_channel_job).await.expect("test result");
        let channel_only = store
            .list(&MemoryScope::global().with_channel("C1"), Page::first(10))
            .await
            .expect("test result");
        assert_eq!(channel_only.len(), 1);
        assert_eq!(channel_only[0].id, channel_job.id);
    }

    #[tokio::test]
    async fn update_applies_partial_fields() {
        let store = MemoryCronStore::default();
        let job = make_job(Utc::now(), MemoryScope::global());
        store.create(&job).await.expect("test result");
        let update = CronJobUpdate {
            enabled: Some(false),
            prompt: Some("new prompt".into()),
            ..Default::default()
        };
        let applied = store.update(&job.id, &update).await.expect("test result");
        assert!(applied);
        let got = store
            .get(&job.id)
            .await
            .expect("test result")
            .expect("test result");
        assert!(!got.enabled);
        assert_eq!(got.prompt, "new prompt");
        assert_eq!(got.name, "ping");
    }

    #[tokio::test]
    async fn update_unknown_returns_false() {
        let store = MemoryCronStore::default();
        let applied = store
            .update(&CronJobId::new(), &CronJobUpdate::default())
            .await
            .expect("test result");
        assert!(!applied);
    }

    #[tokio::test]
    async fn claim_due_picks_only_due_and_unclaimed_jobs() {
        let store = MemoryCronStore::default();
        let now = Utc::now();
        let due = make_job(now - Duration::seconds(1), MemoryScope::global());
        let future = CronJob {
            id: CronJobId::new(),
            next_run: now + Duration::hours(1),
            ..make_job(now, MemoryScope::global())
        };
        store.create(&due).await.expect("test result");
        store.create(&future).await.expect("test result");
        let claimed = store.claim_due(now, 5).await.expect("test result");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, due.id);
        let again = store.claim_due(now, 5).await.expect("test result");
        assert!(again.is_empty(), "second claim returns no jobs");
    }

    #[tokio::test]
    async fn finish_claim_clears_claimed_and_advances_next_run() {
        let store = MemoryCronStore::default();
        let now = Utc::now();
        let job = make_job(now - Duration::seconds(1), MemoryScope::global());
        store.create(&job).await.expect("test result");
        let _ = store.claim_due(now, 1).await.expect("test result");
        let next = now + Duration::seconds(60);
        store
            .finish_claim(&job.id, now, next, false)
            .await
            .expect("test result");
        let got = store
            .get(&job.id)
            .await
            .expect("test result")
            .expect("test result");
        assert!(got.claimed_at.is_none());
        assert_eq!(got.last_run, Some(now));
        assert_eq!(got.next_run, next);
    }

    #[tokio::test]
    async fn finish_claim_disables_run_once_when_requested() {
        let store = MemoryCronStore::default();
        let mut job = make_job(Utc::now(), MemoryScope::global());
        job.run_once = true;
        store.create(&job).await.expect("test result");
        store
            .finish_claim(&job.id, Utc::now(), Utc::now(), true)
            .await
            .expect("test result");
        let got = store
            .get(&job.id)
            .await
            .expect("test result")
            .expect("test result");
        assert!(!got.enabled);
    }

    #[tokio::test]
    async fn release_claim_only_preserves_last_run() {
        let store = MemoryCronStore::default();
        let now = Utc::now();
        let mut job = make_job(now - Duration::seconds(1), MemoryScope::global());
        job.last_run = Some(now - Duration::seconds(120));
        store.create(&job).await.expect("test result");
        let _ = store.claim_due(now, 1).await.expect("test result");
        store
            .release_claim_only(&job.id)
            .await
            .expect("test result");
        let got = store
            .get(&job.id)
            .await
            .expect("test result")
            .expect("test result");
        assert!(got.claimed_at.is_none());
        assert_eq!(got.last_run, job.last_run);
        assert_eq!(got.next_run, job.next_run);
    }

    #[tokio::test]
    async fn recover_stuck_clears_old_claims() {
        let store = MemoryCronStore::default();
        let mut job = make_job(Utc::now(), MemoryScope::global());
        job.claimed_at = Some(
            Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
                .single()
                .expect("valid test datetime"),
        );
        store.create(&job).await.expect("test result");
        let recovered = store.recover_stuck(60).await.expect("test result");
        assert_eq!(recovered, vec![job.id.clone()]);
        let got = store
            .get(&job.id)
            .await
            .expect("test result")
            .expect("test result");
        assert!(got.claimed_at.is_none());
    }
}
