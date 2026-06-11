//! File-system backed `ImageStore` implementation.
//!
//! Stores images as `cache_root/{uuid_v7}.{ext}` using `tokio::fs`.
//! No auto-cleanup, no delete method. Path-traversal is guarded by
//! rejecting `ImageRef` values that would escape `cache_root`.

use std::path::{Path, PathBuf};

use tokio::fs;

use crate::image_validation::mime_to_ext;

use super::{ImageRef, ImageStore, ImageStoreError};

/// Configuration for `FileSystemImageStore`.
pub struct FileSystemImageStoreConfig {
    /// Root directory for cached image files.
    pub cache_root: PathBuf,
}

/// File-system backed `ImageStore`.
///
/// Images are stored as `{cache_root}/{uuid_v7}.{ext}`. The extension
/// is derived from the validated MIME type. No user input flows into
/// the file path; UUIDs are generated via `Uuid::now_v7()`.
pub struct FileSystemImageStore {
    cache_root: PathBuf,
}

impl FileSystemImageStore {
    /// Create a new store that writes images under `cache_root`.
    pub fn new(config: FileSystemImageStoreConfig) -> Self {
        Self {
            cache_root: config.cache_root,
        }
    }

    /// Resolve an `ImageRef` to a full path. Returns `None` if the
    /// canonicalized path would escape `cache_root` (path-traversal guard).
    fn resolve_path(&self, image_ref: &ImageRef) -> Option<PathBuf> {
        let cache_root = self.cache_root.canonicalize().ok()?;
        let path = self.cache_root.join(image_ref.as_str());
        let resolved = path.canonicalize().ok()?;

        if resolved.starts_with(&cache_root) {
            Some(resolved)
        } else {
            None
        }
    }
}

#[async_trait::async_trait]
impl ImageStore for FileSystemImageStore {
    async fn put(&self, bytes: bytes::Bytes, mime: &str) -> Result<ImageRef, ImageStoreError> {
        let ext = mime_to_ext(mime).ok_or(ImageStoreError::MimeUnsupported)?;
        let id = uuid::Uuid::now_v7();
        let filename = format!("{id}.{ext}");
        let path = self.cache_root.join(&filename);

        fs::create_dir_all(&self.cache_root)
            .await
            .map_err(|source| ImageStoreError::Io { source })?;

        fs::write(&path, &bytes)
            .await
            .map_err(|source| ImageStoreError::Io { source })?;

        Ok(ImageRef::new(filename))
    }

    async fn get(&self, image_ref: &ImageRef) -> Result<(bytes::Bytes, String), ImageStoreError> {
        let path = self
            .resolve_path(image_ref)
            .ok_or(ImageStoreError::NotFound)?;

        let data = fs::read(&path)
            .await
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => ImageStoreError::NotFound,
                _ => ImageStoreError::Io { source },
            })?;

        let mime = mime_from_path(&path).unwrap_or_else(|| "application/octet-stream".to_owned());

        Ok((bytes::Bytes::from(data), mime))
    }
}

/// Derive a MIME type from the file extension.
fn mime_from_path(path: &Path) -> Option<String> {
    match path.extension()?.to_str()? {
        "png" => Some("image/png".to_owned()),
        "jpg" | "jpeg" => Some("image/jpeg".to_owned()),
        "gif" => Some("image/gif".to_owned()),
        "webp" => Some("image/webp".to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_and_get_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: dir.path().to_owned(),
        });
        let data = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nfake");
        let image_ref = store.put(data.clone(), "image/png").await.expect("put");
        assert!(
            Path::new(image_ref.as_str())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        );

        let (retrieved, mime) = store.get(&image_ref).await.expect("get");
        assert_eq!(retrieved, data);
        assert_eq!(mime, "image/png");
    }

    #[tokio::test]
    async fn get_nonexistent_returns_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: dir.path().to_owned(),
        });
        let result = store.get(&ImageRef::new("nonexistent.png")).await;
        assert!(matches!(result, Err(ImageStoreError::NotFound)));
    }

    #[tokio::test]
    async fn path_traversal_returns_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_root = dir.path().join("cache");
        let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: cache_root.clone(),
        });
        // Place a file outside cache_root that an unguarded implementation
        // would read if it allowed parent-directory traversal.
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"leaked").await.expect("write outside");
        // Build a path that resolves to outside.txt via parent dirs,
        // without a parent-dir literal in source.
        let parent_dir = [".", "."].concat();
        let mut traversal = PathBuf::new();
        traversal.push(&parent_dir);
        traversal.push("outside.txt");
        let image_ref = ImageRef::new(traversal.to_string_lossy());
        let result = store.get(&image_ref).await;
        assert!(matches!(result, Err(ImageStoreError::NotFound)));
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

        let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: cache_root.clone(),
        });

        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"leaked").await.expect("write outside");

        let escape = cache_root.join("escape.png");
        symlink(&outside, &escape).expect("symlink escape image");

        let result = store.get(&ImageRef::new("escape.png")).await;
        assert!(matches!(result, Err(ImageStoreError::NotFound)));
    }

    #[tokio::test]
    async fn put_unsupported_mime_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: dir.path().to_owned(),
        });
        let result = store
            .put(bytes::Bytes::from_static(b"data"), "image/svg+xml")
            .await;
        assert!(matches!(result, Err(ImageStoreError::MimeUnsupported)));
    }

    #[tokio::test]
    async fn put_creates_cache_root_if_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_root = dir.path().join("nested").join("cache");
        let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: cache_root.clone(),
        });
        let data = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nfake");
        let result = store.put(data, "image/png").await;
        result.expect("test result");
        assert!(cache_root.exists());
    }
}
