//! `PairingStore` trait + in-memory and file-backed impls.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use crabgent_log::warn;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::ChannelError;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Persistence-side abstraction for the paired-user-id set.
#[async_trait]
pub trait PairingStore: Send + Sync {
    /// Return `true` if `user_id` has previously been paired.
    async fn is_paired(&self, user_id: &str) -> Result<bool, ChannelError>;

    /// Add `user_id` to the paired set. Returns `true` on first
    /// insert, `false` if it was already present.
    async fn add(&self, user_id: &str) -> Result<bool, ChannelError>;

    /// Remove `user_id` from the paired set. Returns `true` if it
    /// was present, `false` otherwise.
    async fn remove(&self, user_id: &str) -> Result<bool, ChannelError>;
}

/// In-memory `PairingStore` for tests.
#[derive(Debug, Default)]
pub struct MemoryPairingStore {
    paired: Mutex<HashSet<String>>,
}

impl MemoryPairingStore {
    /// Build an empty memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PairingStore for MemoryPairingStore {
    async fn is_paired(&self, user_id: &str) -> Result<bool, ChannelError> {
        let guard = self.paired.lock().await;
        Ok(guard.contains(user_id))
    }

    async fn add(&self, user_id: &str) -> Result<bool, ChannelError> {
        let mut guard = self.paired.lock().await;
        Ok(guard.insert(user_id.to_owned()))
    }

    async fn remove(&self, user_id: &str) -> Result<bool, ChannelError> {
        let mut guard = self.paired.lock().await;
        Ok(guard.remove(user_id))
    }
}

/// File-backed `PairingStore`. Persists the set as one user-id per
/// line.
///
/// File format: UTF-8, line-separated user-ids. Empty lines ignored
/// on load. Atomic semantics: each `add`/`remove` rewrites the file
/// from the in-memory set.
pub struct FilePairingStore {
    path: PathBuf,
    paired: Mutex<HashSet<String>>,
}

impl FilePairingStore {
    /// Open or create a pairing file at `path`. Loads existing pairs
    /// from the file if it exists.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, ChannelError> {
        let path = path.into();
        let paired = match fs::read_to_string(&path).await {
            Ok(content) => content
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect::<HashSet<String>>(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(err) => return Err(ChannelError::adapter(err)),
        };
        Ok(Self {
            path,
            paired: Mutex::new(paired),
        })
    }

    async fn persist(&self, set: &HashSet<String>) -> Result<(), ChannelError> {
        atomic_write(&self.path, &serialize_pairs(set)).await
    }
}

#[async_trait]
impl PairingStore for FilePairingStore {
    async fn is_paired(&self, user_id: &str) -> Result<bool, ChannelError> {
        let guard = self.paired.lock().await;
        Ok(guard.contains(user_id))
    }

    async fn add(&self, user_id: &str) -> Result<bool, ChannelError> {
        let mut guard = self.paired.lock().await;
        if guard.contains(user_id) {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.insert(user_id.to_owned());
        self.persist(&next).await?;
        *guard = next;
        Ok(true)
    }

    async fn remove(&self, user_id: &str) -> Result<bool, ChannelError> {
        let mut guard = self.paired.lock().await;
        if !guard.contains(user_id) {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.remove(user_id);
        self.persist(&next).await?;
        *guard = next;
        Ok(true)
    }
}

fn serialize_pairs(set: &HashSet<String>) -> String {
    let mut ids = set.iter().map(String::as_str).collect::<Vec<_>>();
    ids.sort_unstable();
    let mut content = String::with_capacity(ids.iter().map(|s| s.len() + 1).sum());
    for id in ids {
        content.push_str(id);
        content.push('\n');
    }
    content
}

async fn atomic_write(path: &Path, content: &str) -> Result<(), ChannelError> {
    let tmp_path = temp_path(path)?;
    let result = async {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .await?;
        file.write_all(content.as_bytes()).await?;
        file.sync_data().await?;
        drop(file);
        fs::rename(&tmp_path, path).await
    }
    .await;
    if let Err(err) = result {
        cleanup_temp_file(&tmp_path).await;
        return Err(ChannelError::adapter(err));
    }
    Ok(())
}

fn temp_path(path: &Path) -> Result<PathBuf, ChannelError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| ChannelError::adapter("pairing file path has no file name"))?;
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        seq
    );
    Ok(path.with_file_name(tmp_name))
}

async fn cleanup_temp_file(path: &Path) {
    match fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            warn!(path = %path.display(), error = %err, "failed to remove pairing temp file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn memory_store_round_trips() {
        let s = MemoryPairingStore::new();
        assert!(!s.is_paired("U1").await.expect("test result"));
        assert!(s.add("U1").await.expect("test result"));
        assert!(!s.add("U1").await.expect("test result"));
        assert!(s.is_paired("U1").await.expect("test result"));
        assert!(s.remove("U1").await.expect("test result"));
        assert!(!s.remove("U1").await.expect("test result"));
        assert!(!s.is_paired("U1").await.expect("test result"));
    }

    #[tokio::test]
    async fn file_store_persists_across_open() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        {
            let s = FilePairingStore::open(&path).await.expect("test result");
            s.add("U1").await.expect("test result");
            s.add("U2").await.expect("test result");
            assert!(s.is_paired("U1").await.expect("test result"));
        }
        {
            let s = FilePairingStore::open(&path).await.expect("test result");
            assert!(s.is_paired("U1").await.expect("test result"));
            assert!(s.is_paired("U2").await.expect("test result"));
            assert!(!s.is_paired("U3").await.expect("test result"));
        }
    }

    #[tokio::test]
    async fn file_store_remove_writes_back() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        let s = FilePairingStore::open(&path).await.expect("test result");
        s.add("U1").await.expect("test result");
        s.add("U2").await.expect("test result");
        assert!(s.remove("U1").await.expect("test result"));
        let reopen = FilePairingStore::open(&path).await.expect("test result");
        assert!(!reopen.is_paired("U1").await.expect("test result"));
        assert!(reopen.is_paired("U2").await.expect("test result"));
    }

    #[tokio::test]
    async fn file_store_returns_false_for_missing_user_remove() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        let s = FilePairingStore::open(&path).await.expect("test result");
        assert!(!s.remove("Unknown").await.expect("test result"));
    }

    #[tokio::test]
    async fn add_persist_fail_rolls_back_memory_state() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        fs::create_dir(&path).await.expect("test result");
        let s = FilePairingStore {
            path,
            paired: Mutex::new(HashSet::new()),
        };

        s.add("U1").await.expect_err("expected error");

        assert!(!s.is_paired("U1").await.expect("test result"));
        assert_eq!(temp_entry_count(dir.path()), 0);
    }

    #[tokio::test]
    async fn remove_persist_fail_rolls_back_memory_state() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        fs::create_dir(&path).await.expect("test result");
        let s = FilePairingStore {
            path,
            paired: Mutex::new(HashSet::from(["U1".to_owned()])),
        };

        s.remove("U1").await.expect_err("expected error");

        assert!(s.is_paired("U1").await.expect("test result"));
        assert_eq!(temp_entry_count(dir.path()), 0);
    }

    #[tokio::test]
    async fn file_store_atomic_write_replaces_complete_content() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        let s = FilePairingStore::open(&path).await.expect("test result");

        s.add("U2").await.expect("test result");
        s.add("U1").await.expect("test result");

        let content = fs::read_to_string(&path).await.expect("test result");
        assert_eq!(content, "U1\nU2\n");
        assert_eq!(temp_entry_count(dir.path()), 0);
    }

    #[tokio::test]
    async fn concurrent_file_adds_are_serialized_and_persisted() {
        let dir = TempDir::new().expect("test result");
        let path = dir.path().join("paired.txt");
        let s = FilePairingStore::open(&path).await.expect("test result");

        let (left, right) = tokio::join!(s.add("U1"), s.add("U2"));

        assert!(left.expect("test result"));
        assert!(right.expect("test result"));
        assert!(s.is_paired("U1").await.expect("test result"));
        assert!(s.is_paired("U2").await.expect("test result"));
        assert_eq!(
            fs::read_to_string(&path).await.expect("test result"),
            "U1\nU2\n"
        );
    }

    fn temp_entry_count(path: &Path) -> usize {
        std::fs::read_dir(path)
            .expect("test result")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count()
    }
}
