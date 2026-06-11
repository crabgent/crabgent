use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::de::Error as _;
use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{AUDIO_PAYLOAD_MAX_BYTES, FILE_PAYLOAD_MAX_BYTES, IMAGE_PAYLOAD_MAX_BYTES};

const IMAGE_PAYLOAD_MAX_WIRE_DATA_BYTES: usize = IMAGE_PAYLOAD_MAX_BYTES * 4 / 3 + 16;
const AUDIO_PAYLOAD_MAX_WIRE_DATA_BYTES: usize = AUDIO_PAYLOAD_MAX_BYTES * 4 / 3 + 16;
const FILE_PAYLOAD_MAX_WIRE_DATA_BYTES: usize = FILE_PAYLOAD_MAX_BYTES * 4 / 3 + 16;
const IMAGE_PAYLOAD_ALLOWED_MIMES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];
pub const AUDIO_PAYLOAD_ALLOWED_MIMES: &[&str] = &[
    "audio/ogg",
    "audio/aac",
    "audio/mpeg",
    "audio/mp3",
    "audio/mp4",
    "audio/x-m4a",
    "audio/wav",
    "audio/webm",
    "audio/flac",
    "audio/x-flac",
    "audio/opus",
    "audio/L16",
];
const FILE_PAYLOAD_ALLOWED_MIMES: &[&str] = &["text/plain"];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PayloadError {
    #[error("unsupported MIME type: {mime}")]
    UnsupportedMime { mime: String },
    #[error("{kind} payload exceeds {max_bytes} bytes")]
    TooLarge {
        kind: &'static str,
        max_bytes: usize,
    },
}

/// Image bytes plus MIME type in provider-neutral form.
#[derive(Clone, PartialEq, Eq)]
pub struct ImagePayload {
    bytes: Arc<[u8]>,
    mime: String,
}

impl ImagePayload {
    pub fn new(bytes: impl Into<Arc<[u8]>>, mime: impl Into<String>) -> Result<Self, PayloadError> {
        let bytes = bytes.into();
        let mime = mime.into();
        validate_payload(
            "image",
            bytes.len(),
            &mime,
            IMAGE_PAYLOAD_ALLOWED_MIMES,
            IMAGE_PAYLOAD_MAX_BYTES,
        )?;
        Ok(Self { bytes, mime })
    }

    #[must_use]
    pub const fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    #[must_use]
    pub(super) fn encoded_data(&self) -> String {
        encode_bytes(self.bytes.as_ref())
    }
}

impl fmt::Debug for ImagePayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImagePayload")
            .field("bytes", &ByteLen(self.bytes.len()))
            .field("mime", &self.mime)
            .finish()
    }
}

impl Serialize for ImagePayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ImagePayload", 2)?;
        state.serialize_field("mime", &self.mime)?;
        state.serialize_field("data", &self.encoded_data())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ImagePayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ImagePayloadWire {
            mime: String,
            data: String,
        }

        let wire = ImagePayloadWire::deserialize(deserializer)?;
        let bytes = decode_limited::<D::Error>(
            wire.data,
            IMAGE_PAYLOAD_MAX_WIRE_DATA_BYTES,
            IMAGE_PAYLOAD_MAX_BYTES,
            &wire.mime,
            IMAGE_PAYLOAD_ALLOWED_MIMES,
            "image",
        )?;
        Self::new(bytes, wire.mime).map_err(D::Error::custom)
    }
}

/// Audio bytes plus MIME type in provider-neutral form.
#[derive(Clone, PartialEq, Eq)]
pub struct AudioPayload {
    bytes: Arc<[u8]>,
    mime: String,
    pub filename: Option<String>,
}

impl AudioPayload {
    pub fn new(
        bytes: impl Into<Arc<[u8]>>,
        mime: impl Into<String>,
        filename: Option<String>,
    ) -> Result<Self, PayloadError> {
        let bytes = bytes.into();
        let mime = mime.into();
        validate_payload(
            "audio",
            bytes.len(),
            &mime,
            AUDIO_PAYLOAD_ALLOWED_MIMES,
            AUDIO_PAYLOAD_MAX_BYTES,
        )?;
        Ok(Self {
            bytes,
            mime,
            filename,
        })
    }

    #[must_use]
    pub const fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    #[must_use]
    pub(super) fn encoded_data(&self) -> String {
        encode_bytes(self.bytes.as_ref())
    }
}

impl fmt::Debug for AudioPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioPayload")
            .field("bytes", &ByteLen(self.bytes.len()))
            .field("mime", &self.mime)
            .field("filename", &self.filename)
            .finish()
    }
}

impl Serialize for AudioPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("AudioPayload", 3)?;
        state.serialize_field("mime", &self.mime)?;
        state.serialize_field("data", &self.encoded_data())?;
        state.serialize_field("filename", &self.filename)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for AudioPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct AudioPayloadWire {
            mime: String,
            data: String,
            filename: Option<String>,
        }

        let wire = AudioPayloadWire::deserialize(deserializer)?;
        let bytes = decode_limited::<D::Error>(
            wire.data,
            AUDIO_PAYLOAD_MAX_WIRE_DATA_BYTES,
            AUDIO_PAYLOAD_MAX_BYTES,
            &wire.mime,
            AUDIO_PAYLOAD_ALLOWED_MIMES,
            "audio",
        )?;
        Self::new(bytes, wire.mime, wire.filename).map_err(D::Error::custom)
    }
}

/// Generic file bytes plus MIME type in provider-neutral form.
#[derive(Clone, PartialEq, Eq)]
pub struct FilePayload {
    bytes: Arc<[u8]>,
    mime: String,
    pub filename: String,
}

impl FilePayload {
    pub fn new(
        bytes: impl Into<Arc<[u8]>>,
        mime: impl Into<String>,
        filename: impl Into<String>,
    ) -> Result<Self, PayloadError> {
        let bytes = bytes.into();
        let mime = mime.into();
        validate_payload(
            "file",
            bytes.len(),
            &mime,
            FILE_PAYLOAD_ALLOWED_MIMES,
            FILE_PAYLOAD_MAX_BYTES,
        )?;
        Ok(Self {
            bytes,
            mime,
            filename: filename.into(),
        })
    }

    #[must_use]
    pub const fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    #[must_use]
    pub(super) fn encoded_data(&self) -> String {
        encode_bytes(self.bytes.as_ref())
    }
}

impl fmt::Debug for FilePayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilePayload")
            .field("bytes", &ByteLen(self.bytes.len()))
            .field("mime", &self.mime)
            .field("filename", &self.filename)
            .finish()
    }
}

impl Serialize for FilePayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("FilePayload", 3)?;
        state.serialize_field("mime", &self.mime)?;
        state.serialize_field("data", &self.encoded_data())?;
        state.serialize_field("filename", &self.filename)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for FilePayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct FilePayloadWire {
            mime: String,
            data: String,
            filename: String,
        }

        let wire = FilePayloadWire::deserialize(deserializer)?;
        let bytes = decode_limited::<D::Error>(
            wire.data,
            FILE_PAYLOAD_MAX_WIRE_DATA_BYTES,
            FILE_PAYLOAD_MAX_BYTES,
            &wire.mime,
            FILE_PAYLOAD_ALLOWED_MIMES,
            "file",
        )?;
        Self::new(bytes, wire.mime, wire.filename).map_err(D::Error::custom)
    }
}

#[must_use]
pub(super) fn encode_bytes(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

fn decode_limited<E>(
    data: String,
    max_wire_bytes: usize,
    max_payload_bytes: usize,
    mime: &str,
    allowed_mimes: &[&str],
    kind: &'static str,
) -> Result<Arc<[u8]>, E>
where
    E: serde::de::Error,
{
    if data.len() > max_wire_bytes {
        return Err(E::custom(format!(
            "{kind} payload exceeds {max_payload_bytes} bytes"
        )));
    }
    let bytes = BASE64_STANDARD.decode(data).map_err(E::custom)?;
    validate_payload(kind, bytes.len(), mime, allowed_mimes, max_payload_bytes)
        .map_err(E::custom)?;
    Ok(Arc::from(bytes))
}

fn validate_payload(
    kind: &'static str,
    byte_len: usize,
    mime: &str,
    allowed_mimes: &[&str],
    max_payload_bytes: usize,
) -> Result<(), PayloadError> {
    if !allowed_mimes.contains(&mime) {
        return Err(PayloadError::UnsupportedMime {
            mime: mime.to_owned(),
        });
    }
    if byte_len > max_payload_bytes {
        return Err(PayloadError::TooLarge {
            kind,
            max_bytes: max_payload_bytes,
        });
    }
    Ok(())
}

struct ByteLen(usize);

impl fmt::Debug for ByteLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "len={}", self.0)
    }
}

#[cfg(test)]
mod tests;
