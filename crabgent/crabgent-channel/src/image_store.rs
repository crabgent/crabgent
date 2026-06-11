//! Image store trait and supporting types for vision support.
//!
//! Provides `ImageStore` (put/get), `ImageRef` (opaque handle to a
//! cached image), and `ImageStoreError`. The concrete file-system
//! implementation lives in the `file_system` sub-module.

pub mod file_system;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Opaque reference to a cached image.
///
/// The inner string is a UUID v7 identifier set by the store
/// implementation. Callers must not construct `ImageRef` directly;
/// they receive one from `ImageStore::put`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageRef(String);

impl ImageRef {
    /// Create a new `ImageRef` from a store-generated identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Return the inner identifier string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors that can occur when interacting with an image store.
#[derive(Debug, Error)]
pub enum ImageStoreError {
    /// An I/O error occurred during a store operation.
    #[error("I/O error: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    /// The requested image was not found in the store.
    #[error("image not found")]
    NotFound,
    /// The image data is invalid (e.g. failed validation).
    #[error("invalid image")]
    Invalid,
    /// The MIME type is not supported by the store.
    #[error("unsupported MIME type")]
    MimeUnsupported,
}

/// Trait for storing and retrieving cached images.
///
/// Implementations must be `Send + Sync` so they can be shared
/// across async tasks. All I/O must be async (`tokio::fs`); synchronous
/// file operations are forbidden outside `#[cfg(test)]`.
#[async_trait::async_trait]
pub trait ImageStore: Send + Sync {
    /// Store the given image bytes and return an opaque reference.
    ///
    /// The `mime` parameter carries the validated MIME type (e.g.
    /// `"image/png"`). Implementations use it to derive the file
    /// extension but must not trust it for magic-byte checks; that
    /// validation happens upstream in `ImageValidator`.
    async fn put(&self, bytes: bytes::Bytes, mime: &str) -> Result<ImageRef, ImageStoreError>;

    /// Retrieve previously stored image bytes and their MIME type.
    async fn get(&self, image_ref: &ImageRef) -> Result<(bytes::Bytes, String), ImageStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_ref_serde_roundtrip() {
        let original = ImageRef::new("01946a3c-7c2b-7d2e-8f1a-5b3c2d1e0f0a");
        let json = serde_json::to_string(&original).expect("serialize");
        // transparent: serializes as plain string
        assert_eq!(json, "\"01946a3c-7c2b-7d2e-8f1a-5b3c2d1e0f0a\"");
        let back: ImageRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, back);
    }

    #[test]
    fn image_ref_as_str_returns_inner() {
        let r = ImageRef::new("test-id");
        assert_eq!(r.as_str(), "test-id");
    }

    #[test]
    fn image_store_error_display_io() {
        let err = ImageStoreError::Io {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        assert_eq!(format!("{err}"), "I/O error: no such file");
    }

    #[test]
    fn image_store_error_display_not_found() {
        let err = ImageStoreError::NotFound;
        assert_eq!(format!("{err}"), "image not found");
    }

    #[test]
    fn image_store_error_display_invalid() {
        let err = ImageStoreError::Invalid;
        assert_eq!(format!("{err}"), "invalid image");
    }

    #[test]
    fn image_store_error_display_mime_unsupported() {
        let err = ImageStoreError::MimeUnsupported;
        assert_eq!(format!("{err}"), "unsupported MIME type");
    }
}
