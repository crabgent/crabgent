//! Size-capped media body assembly shared by channel adapters.

use bytes::{Bytes, BytesMut};

/// Errors shared by the per-adapter inbound media download helpers.
///
/// Every channel adapter (Slack, Matrix, Telegram) reports the same set of
/// download failure modes with generic, credential-free messages. The
/// `Storage` variant only applies to image downloads, where the validated
/// bytes are persisted; audio download paths never construct it.
#[derive(Debug, thiserror::Error)]
pub enum MediaDownloadError {
    /// Authentication failed (401/403).
    #[error("authentication failed")]
    Auth,
    /// Network or protocol error.
    #[error("network error")]
    Network,
    /// Response body exceeded the size cap.
    #[error("response too large")]
    Size,
    /// The response MIME type is not supported.
    #[error("response has unsupported MIME type")]
    Mime,
    /// Storing the validated media failed.
    #[error("storage error")]
    Storage,
}

/// Marker error for media bodies that exceed their configured byte cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaBodyTooLarge;

/// Incremental body builder that rejects oversized media before allocation
/// can grow past the configured cap.
#[derive(Debug)]
pub struct CappedMediaBody {
    max_bytes: usize,
    body: BytesMut,
}

impl CappedMediaBody {
    /// Create a body builder and reject known oversized `Content-Length`
    /// values before any chunks are read.
    pub fn new(max_bytes: u64, content_length: Option<u64>) -> Result<Self, MediaBodyTooLarge> {
        if content_length.is_some_and(|length| length > max_bytes) {
            return Err(MediaBodyTooLarge);
        }
        Ok(Self {
            max_bytes: usize::try_from(max_bytes).unwrap_or(usize::MAX),
            body: BytesMut::new(),
        })
    }

    /// Append one response chunk, returning an error before crossing the cap.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<(), MediaBodyTooLarge> {
        if chunk.len() > self.max_bytes.saturating_sub(self.body.len()) {
            return Err(MediaBodyTooLarge);
        }
        self.body.extend_from_slice(chunk);
        Ok(())
    }

    /// Return the assembled body.
    #[must_use]
    pub fn finish(self) -> Bytes {
        self.body.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_content_length_over_cap_before_chunks() {
        let err = CappedMediaBody::new(4, Some(5)).expect_err("known oversize");

        assert_eq!(err, MediaBodyTooLarge);
    }

    #[test]
    fn accepts_exact_cap() {
        let mut body = CappedMediaBody::new(4, Some(4)).expect("builder");

        body.push_chunk(b"ab").expect("first chunk");
        body.push_chunk(b"cd").expect("second chunk");

        assert_eq!(body.finish(), Bytes::from_static(b"abcd"));
    }

    #[test]
    fn rejects_chunk_that_crosses_cap() {
        let mut body = CappedMediaBody::new(4, None).expect("builder");

        body.push_chunk(b"abc").expect("first chunk");
        let err = body.push_chunk(b"de").expect_err("chunk crosses cap");

        assert_eq!(err, MediaBodyTooLarge);
        assert_eq!(body.finish(), Bytes::from_static(b"abc"));
    }
}
