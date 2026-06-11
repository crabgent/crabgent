//! Media-byte builders shared by channel-adapter inbound tests.
//!
//! These fold the per-file `minimal_*_bytes` helpers that adapter audio/image
//! tests re-declared to feed mock download responses.

/// A minimal Ogg container: the `OggS` capture pattern padded to 64 bytes.
///
/// Enough for the audio validators to accept the sniffed container while
/// keeping the fixture body trivial.
#[must_use]
pub fn minimal_ogg_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(b"OggS");
    bytes.extend_from_slice(&[0_u8; 60]);
    bytes
}
