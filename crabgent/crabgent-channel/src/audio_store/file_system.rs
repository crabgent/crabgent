//! File-system backed `AudioStore` implementation.
//!
//! Stores audio as `cache_root/{uuid_v7}.{ext}` using `tokio::fs`. There
//! is no automatic cleanup beyond an explicit `sweep_expired` call and no
//! single-file delete method. Path-traversal is guarded by rejecting
//! `AudioRef` values that would escape `cache_root`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crabgent_core::{AUDIO_PAYLOAD_MAX_BYTES, AudioRef};
use tokio::fs;

use super::{AudioStore, AudioStoreError};

/// Configuration for `FileSystemAudioStore`.
pub struct FileSystemAudioStoreConfig {
    /// Root directory for cached audio files.
    pub cache_root: PathBuf,
    /// Reject `put` of payloads larger than this many bytes.
    pub max_bytes: usize,
}

impl FileSystemAudioStoreConfig {
    /// Config rooted at `cache_root` with the default 25 MB size cap.
    #[must_use]
    pub const fn new(cache_root: PathBuf) -> Self {
        Self {
            cache_root,
            max_bytes: AUDIO_PAYLOAD_MAX_BYTES,
        }
    }
}

/// File-system backed `AudioStore`.
///
/// Audio is stored as `{cache_root}/{uuid_v7}.{ext}`. The extension is
/// derived from the validated MIME type. No user input flows into the
/// file path; UUIDs are generated via `Uuid::now_v7()`.
pub struct FileSystemAudioStore {
    cache_root: PathBuf,
    max_bytes: usize,
}

impl FileSystemAudioStore {
    /// Create a new store from `config`.
    #[must_use]
    pub fn new(config: FileSystemAudioStoreConfig) -> Self {
        Self {
            cache_root: config.cache_root,
            max_bytes: config.max_bytes,
        }
    }

    /// Resolve an `AudioRef` to a full path. Returns `None` if the
    /// canonicalized path would escape `cache_root` (path-traversal guard).
    ///
    /// Known limitation: there is a TOCTOU window between this check and
    /// the later `fs::read`. Exploiting it requires write access to
    /// `cache_root` itself, so an attacker already holds filesystem write
    /// access at that level. Mirrors `FileSystemImageStore`.
    fn resolve_path(&self, audio_ref: &AudioRef) -> Option<PathBuf> {
        let cache_root = self.cache_root.canonicalize().ok()?;
        let path = self.cache_root.join(audio_ref.as_str());
        let resolved = path.canonicalize().ok()?;

        if resolved.starts_with(&cache_root) {
            Some(resolved)
        } else {
            None
        }
    }
}

/// Inspect one directory entry and remove it if expired. Returns `true` when
/// the file was removed. Stat/unlink failures are logged and return `false`
/// so the sweep continues best-effort.
async fn sweep_one_entry(entry: &fs::DirEntry, now: SystemTime, ttl: Duration) -> bool {
    let Ok(metadata) = entry.metadata().await else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    if !now.duration_since(modified).is_ok_and(|age| age > ttl) {
        return false;
    }
    if let Err(error) = fs::remove_file(entry.path()).await {
        crabgent_log::warn!(%error, "audio store sweep failed to remove expired file");
        return false;
    }
    true
}

#[async_trait::async_trait]
impl AudioStore for FileSystemAudioStore {
    async fn put(&self, bytes: bytes::Bytes, mime: &str) -> Result<AudioRef, AudioStoreError> {
        if bytes.len() > self.max_bytes {
            return Err(AudioStoreError::TooLarge {
                size: bytes.len(),
                max: self.max_bytes,
            });
        }
        let ext = audio_mime_to_ext(mime).ok_or(AudioStoreError::MimeUnsupported)?;
        let id = uuid::Uuid::now_v7();
        let filename = format!("{id}.{ext}");
        let path = self.cache_root.join(&filename);

        fs::create_dir_all(&self.cache_root)
            .await
            .map_err(|source| AudioStoreError::Io { source })?;
        fs::write(&path, &bytes)
            .await
            .map_err(|source| AudioStoreError::Io { source })?;

        Ok(AudioRef::new(filename))
    }

    /// The returned MIME is derived from the stored file extension and is
    /// canonical: `audio/x-flac` is normalized to `audio/flac` and
    /// `audio/x-m4a` to `audio/mp4`, so it may differ from the MIME passed
    /// to `put`.
    async fn get(&self, audio_ref: &AudioRef) -> Result<(bytes::Bytes, String), AudioStoreError> {
        let path = self
            .resolve_path(audio_ref)
            .ok_or(AudioStoreError::NotFound)?;

        let data = fs::read(&path)
            .await
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => AudioStoreError::NotFound,
                _ => AudioStoreError::Io { source },
            })?;

        let mime = mime_from_ext(&path).unwrap_or_else(|| "application/octet-stream".to_owned());
        Ok((bytes::Bytes::from(data), mime))
    }

    /// Delete cached files whose modification time is older than `ttl`.
    ///
    /// Best-effort: entries that cannot be stat-ed or removed are logged and
    /// skipped. Returns the number of removed files. A missing cache root is
    /// treated as empty, not an error. `AudioStoreSweeper` schedules this.
    async fn sweep_expired(&self, ttl: Duration) -> Result<usize, AudioStoreError> {
        let now = SystemTime::now();
        let mut entries = match fs::read_dir(&self.cache_root).await {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => return Err(AudioStoreError::Io { source }),
        };

        let mut removed = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| AudioStoreError::Io { source })?
        {
            if sweep_one_entry(&entry, now, ttl).await {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Map a validated audio MIME type to a file extension.
fn audio_mime_to_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "audio/wav" => Some("wav"),
        "audio/aac" => Some("aac"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/L16" => Some("l16"),
        "audio/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        "audio/webm" => Some("webm"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        _ => None,
    }
}

/// Derive a MIME type from a stored file's extension.
fn mime_from_ext(path: &Path) -> Option<String> {
    match path.extension()?.to_str()? {
        "wav" => Some("audio/wav".to_owned()),
        "aac" => Some("audio/aac".to_owned()),
        "mp3" => Some("audio/mpeg".to_owned()),
        "l16" => Some("audio/L16".to_owned()),
        "ogg" => Some("audio/ogg".to_owned()),
        "opus" => Some("audio/opus".to_owned()),
        "webm" => Some("audio/webm".to_owned()),
        "flac" => Some("audio/flac".to_owned()),
        "m4a" => Some("audio/mp4".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(cache_root: PathBuf) -> FileSystemAudioStore {
        FileSystemAudioStore::new(FileSystemAudioStoreConfig::new(cache_root))
    }

    #[tokio::test]
    async fn put_and_get_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().to_owned());
        let data = bytes::Bytes::from_static(b"RIFF\x00\x00\x00\x00WAVEfake");
        let audio_ref = store.put(data.clone(), "audio/wav").await.expect("put");
        assert!(
            Path::new(audio_ref.as_str())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        );

        let (retrieved, mime) = store.get(&audio_ref).await.expect("get");
        assert_eq!(retrieved, data);
        assert_eq!(mime, "audio/wav");
    }

    #[tokio::test]
    async fn put_and_get_tts_specific_formats() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().to_owned());

        for (mime, ext) in [("audio/aac", "aac"), ("audio/L16", "l16")] {
            let data = bytes::Bytes::from_static(b"tts-audio");
            let audio_ref = store.put(data.clone(), mime).await.expect("put");
            assert!(
                Path::new(audio_ref.as_str())
                    .extension()
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(ext)),
                "stored {mime} with .{ext} extension"
            );

            let (retrieved, retrieved_mime) = store.get(&audio_ref).await.expect("get");
            assert_eq!(retrieved, data);
            assert_eq!(retrieved_mime, mime);
        }
    }

    #[tokio::test]
    async fn get_nonexistent_returns_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().to_owned());
        let result = store.get(&AudioRef::new("nonexistent.wav")).await;
        assert!(matches!(result, Err(AudioStoreError::NotFound)));
    }

    #[tokio::test]
    async fn path_traversal_returns_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_root = dir.path().join("cache");
        let store = store(cache_root);
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"leaked").await.expect("write outside");
        let parent_dir = [".", "."].concat();
        let mut traversal = PathBuf::new();
        traversal.push(&parent_dir);
        traversal.push("outside.txt");
        let audio_ref = AudioRef::new(traversal.to_string_lossy());
        let result = store.get(&audio_ref).await;
        assert!(matches!(result, Err(AudioStoreError::NotFound)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_target_outside_cache_root_is_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let real_cache_root = dir.path().join("real-cache");
        fs::create_dir_all(&real_cache_root)
            .await
            .expect("create real cache root");

        let cache_root = dir.path().join("cache-root");
        symlink(&real_cache_root, &cache_root).expect("symlink cache root");
        let store = store(cache_root.clone());

        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"leaked").await.expect("write outside");

        let escape = cache_root.join("escape.wav");
        symlink(&outside, &escape).expect("symlink escape audio");

        let result = store.get(&AudioRef::new("escape.wav")).await;
        assert!(matches!(result, Err(AudioStoreError::NotFound)));
    }

    #[tokio::test]
    async fn put_unsupported_mime_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().to_owned());
        let result = store
            .put(bytes::Bytes::from_static(b"data"), "audio/aiff")
            .await;
        assert!(matches!(result, Err(AudioStoreError::MimeUnsupported)));
    }

    #[tokio::test]
    async fn put_rejects_oversized_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileSystemAudioStore::new(FileSystemAudioStoreConfig {
            cache_root: dir.path().to_owned(),
            max_bytes: 4,
        });
        let result = store
            .put(bytes::Bytes::from_static(b"toolong"), "audio/wav")
            .await;
        assert!(matches!(
            result,
            Err(AudioStoreError::TooLarge { size: 7, max: 4 })
        ));
    }

    #[tokio::test]
    async fn put_creates_cache_root_if_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_root = dir.path().join("nested").join("cache");
        let store = store(cache_root.clone());
        store
            .put(bytes::Bytes::from_static(b"RIFFfake"), "audio/wav")
            .await
            .expect("put");
        assert!(cache_root.exists());
    }

    #[tokio::test]
    async fn sweep_expired_removes_files_older_than_ttl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().to_owned());
        let audio_ref = store
            .put(bytes::Bytes::from_static(b"RIFFfake"), "audio/wav")
            .await
            .expect("put");
        // ttl = 0: any already-written file is older than zero.
        let removed = store.sweep_expired(Duration::ZERO).await.expect("sweep");
        assert_eq!(removed, 1);
        assert!(matches!(
            store.get(&audio_ref).await,
            Err(AudioStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn sweep_expired_keeps_fresh_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().to_owned());
        let audio_ref = store
            .put(bytes::Bytes::from_static(b"RIFFfake"), "audio/wav")
            .await
            .expect("put");
        let removed = store
            .sweep_expired(Duration::from_hours(1))
            .await
            .expect("sweep");
        assert_eq!(removed, 0);
        store.get(&audio_ref).await.expect("still present");
    }

    #[tokio::test]
    async fn sweep_expired_on_missing_cache_root_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(dir.path().join("absent"));
        let removed = store.sweep_expired(Duration::ZERO).await.expect("sweep");
        assert_eq!(removed, 0);
    }

    #[test]
    fn audio_mime_to_ext_maps_known_and_rejects_unknown() {
        assert_eq!(audio_mime_to_ext("audio/mpeg"), Some("mp3"));
        assert_eq!(audio_mime_to_ext("audio/mp3"), Some("mp3"));
        assert_eq!(audio_mime_to_ext("audio/aac"), Some("aac"));
        assert_eq!(audio_mime_to_ext("audio/L16"), Some("l16"));
        assert_eq!(audio_mime_to_ext("audio/wav"), Some("wav"));
        assert_eq!(audio_mime_to_ext("audio/x-flac"), Some("flac"));
        assert_eq!(audio_mime_to_ext("audio/x-m4a"), Some("m4a"));
        assert_eq!(audio_mime_to_ext("application/json"), None);
    }
}
