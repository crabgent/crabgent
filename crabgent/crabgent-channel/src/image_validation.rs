//! Image validation: size cap, MIME whitelist, magic-byte check.
//!
//! Every inbound image must pass through `ImageValidator` before it
//! reaches `ContentBlock::Image`. The validation sequence is:
//! 1. Byte length against `MAX_IMAGE_BYTES`
//! 2. Magic-byte check via `image::guess_format()`
//! 3. Declared MIME against the allowed whitelist
//! 4. Dimensions check (if the provider mandates limits)

/// Maximum allowed inbound image size in bytes (5 MB).
///
/// This channel-layer cap mirrors
/// `crabgent_core::message::IMAGE_PAYLOAD_MAX_BYTES` while keeping a separate
/// `u64` type for adapter validation and rejection messages.
pub const MAX_IMAGE_BYTES: u64 = 5_000_000;

/// Allowed MIME types for inbound images.
pub const ALLOWED_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Map a validated MIME type to a file extension (without dot).
#[must_use]
pub fn mime_to_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Reasons an image can be rejected during validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRejection {
    /// The image exceeds the maximum allowed size.
    TooLarge { bytes: u64 },
    /// The declared MIME type is not in the whitelist.
    UnsupportedMime { mime: String },
    /// The image bytes could not be identified as a known format.
    InvalidBytes,
    /// The image dimensions exceed the allowed maximum.
    DimensionsExceeded { w: u32, h: u32 },
}

impl std::fmt::Display for ImageRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { bytes } => {
                write!(f, "[image too large: {bytes} bytes, max {MAX_IMAGE_BYTES}]")
            }
            Self::UnsupportedMime { mime } => {
                write!(f, "unsupported MIME type: {mime}")
            }
            Self::InvalidBytes => write!(f, "image bytes not recognized as a valid format"),
            Self::DimensionsExceeded { w, h } => {
                write!(f, "image dimensions exceeded: {w}x{h}")
            }
        }
    }
}

impl std::error::Error for ImageRejection {}

/// LLM-safe text fallback for an image rejected before it can become a
/// `ContentBlock::Image`.
#[must_use]
pub fn image_rejection_fallback_text(rejection: &ImageRejection) -> String {
    match rejection {
        ImageRejection::TooLarge { .. } => rejection.to_string(),
        _ => format!("[image rejected: {rejection}]"),
    }
}

/// Validates inbound image bytes against size, MIME, and magic-byte rules.
pub struct ImageValidator;

impl ImageValidator {
    /// Create a new validator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Validate image bytes and declared MIME.
    ///
    /// Returns the canonical MIME type (e.g. `"image/png"`) on success, or an
    /// `ImageRejection` describing why the image was rejected.
    pub fn validate(
        &self,
        bytes: &[u8],
        declared_mime: &str,
    ) -> Result<&'static str, ImageRejection> {
        // Step 1: size check
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > MAX_IMAGE_BYTES {
            return Err(ImageRejection::TooLarge { bytes: len });
        }

        // Step 2: magic-byte check
        let guessed_mime = mime_from_image_bytes(bytes)?;

        // Step 3: MIME whitelist
        if !ALLOWED_MIMES.contains(&declared_mime) {
            return Err(ImageRejection::UnsupportedMime {
                mime: declared_mime.to_owned(),
            });
        }

        // Step 4: cross-check guessed format against declared MIME
        if guessed_mime != declared_mime {
            return Err(ImageRejection::InvalidBytes);
        }

        Ok(guessed_mime)
    }
}

/// Detect the canonical MIME type for image bytes from magic bytes.
pub fn mime_from_image_bytes(bytes: &[u8]) -> Result<&'static str, ImageRejection> {
    let guessed_format = image::guess_format(bytes).map_err(|err| {
        crabgent_log::debug!(error = %err, "image magic-byte check failed");
        ImageRejection::InvalidBytes
    })?;
    Ok(format_from_image_format(guessed_format))
}

/// Convert an `image::ImageFormat` to its canonical MIME type string.
///
/// The `_ => "unknown"` arm is required because `image::ImageFormat` is
/// `#[non_exhaustive]`. It cannot reach `Ok(...)` in `validate()`: step 4
/// rejects any `(guessed_mime != declared_mime)` mismatch, and no adapter
/// passes `"unknown"` as `declared_mime` (validated against `ALLOWED_MIMES`
/// in step 3). If `ALLOWED_MIMES` ever gains a new entry, extend this match
/// arm in lockstep.
const fn format_from_image_format(format: image::ImageFormat) -> &'static str {
    match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_rejected_too_large() {
        let validator = ImageValidator::new();
        let size = usize::try_from(MAX_IMAGE_BYTES + 1).expect("5 MB cap fits usize");
        let big = vec![0u8; size];
        let result = validator.validate(&big, "image/png");
        assert!(matches!(result, Err(ImageRejection::TooLarge { .. })));
    }

    #[test]
    fn fallback_text_keeps_size_reject_shape() {
        let text = image_rejection_fallback_text(&ImageRejection::TooLarge { bytes: 42 });
        assert_eq!(text, "[image too large: 42 bytes, max 5000000]");
    }

    #[test]
    fn fallback_text_wraps_non_size_rejects() {
        let text = image_rejection_fallback_text(&ImageRejection::InvalidBytes);
        assert_eq!(
            text,
            "[image rejected: image bytes not recognized as a valid format]"
        );
    }

    #[test]
    fn validate_rejects_unsupported_mime() {
        // Valid PNG bytes but declared as image/svg+xml: magic-byte passes
        // but MIME whitelist rejects.
        let validator = ImageValidator::new();
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(
            b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
        );
        let result = validator.validate(&png, "image/svg+xml");
        assert!(
            matches!(result, Err(ImageRejection::UnsupportedMime { mime } ) if mime == "image/svg+xml")
        );
    }

    #[test]
    fn validate_rejects_invalid_bytes() {
        let validator = ImageValidator::new();
        let result = validator.validate(b"not an image", "image/png");
        assert!(matches!(result, Err(ImageRejection::InvalidBytes)));
    }

    #[test]
    fn validate_rejects_invalid_bytes_before_mime_check() {
        // Invalid bytes with unsupported MIME: magic-byte check runs first
        // per vision.md sequence, so result is InvalidBytes not UnsupportedMime.
        let validator = ImageValidator::new();
        let result = validator.validate(b"not an image", "image/svg+xml");
        assert!(matches!(result, Err(ImageRejection::InvalidBytes)));
    }

    #[test]
    fn image_rejected_mime_mismatch() {
        // PNG magic bytes but declared as image/gif
        let validator = ImageValidator::new();
        let png_magic = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let result = validator.validate(png_magic, "image/gif");
        assert!(matches!(result, Err(ImageRejection::InvalidBytes)));
    }

    #[test]
    fn validate_accepts_valid_png() {
        let validator = ImageValidator::new();
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG magic
        // Add minimal IHDR chunk to satisfy image::guess_format
        png.extend_from_slice(
            b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
        );
        let result = validator.validate(&png, "image/png");
        result.expect("test result");
    }

    #[test]
    fn too_large_rejection_display_includes_sizes() {
        let r = ImageRejection::TooLarge { bytes: 6_000_000 };
        let msg = format!("{r}");
        assert!(msg.contains("6000000"), "{msg}");
        assert!(msg.contains("5000000"), "{msg}");
    }

    #[test]
    fn max_image_bytes_is_five_million() {
        assert_eq!(MAX_IMAGE_BYTES, 5_000_000);
    }

    #[test]
    fn allowed_mimes_has_four_entries() {
        assert_eq!(ALLOWED_MIMES.len(), 4);
    }

    #[test]
    fn mime_to_ext_roundtrip() {
        assert_eq!(mime_to_ext("image/png"), Some("png"));
        assert_eq!(mime_to_ext("image/jpeg"), Some("jpg"));
        assert_eq!(mime_to_ext("image/gif"), Some("gif"));
        assert_eq!(mime_to_ext("image/webp"), Some("webp"));
        assert_eq!(mime_to_ext("image/svg+xml"), None);
    }
}
