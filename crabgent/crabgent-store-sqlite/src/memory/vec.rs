//! `SQLite` vector BLOB encoding helpers.
//!
//! Embeddings are stored as little-endian IEEE-754 `f32` so that databases
//! written on one host can be opened on another regardless of the writer's
//! native endianness. `to_ne_bytes` / `from_ne_bytes` would round-trip
//! correctly on a single host but would produce mojibake blobs when a file
//! travels between little-endian and big-endian deployments.

use crabgent_store::StoreError;

pub(super) fn encode_embedding(vector: &[f32]) -> Result<Vec<u8>, StoreError> {
    if vector.is_empty() {
        return Err(StoreError::invalid("embedding vector must not be empty"));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(StoreError::invalid(
            "embedding vector must contain only finite values",
        ));
    }

    let mut bytes = Vec::with_capacity(size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

pub(super) fn decode_embedding(blob: Option<Vec<u8>>) -> Result<Option<Vec<f32>>, StoreError> {
    let Some(blob) = blob else {
        return Ok(None);
    };
    if blob.is_empty() || blob.len() % size_of::<f32>() != 0 {
        return Err(StoreError::backend("invalid embedding blob length"));
    }

    let mut vector = Vec::with_capacity(blob.len() / size_of::<f32>());
    for chunk in blob.chunks_exact(size_of::<f32>()) {
        let bytes = <[u8; 4]>::try_from(chunk)
            .map_err(|err| StoreError::backend(format!("invalid embedding blob chunk: {err}")))?;
        vector.push(f32::from_le_bytes(bytes));
    }
    Ok(Some(vector))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_embedding_uses_little_endian_layout() {
        // 1.0_f32 = 0x3F800000; little-endian byte order is 00 00 80 3F.
        let bytes = encode_embedding(&[1.0_f32]).expect("encode 1.0");
        assert_eq!(bytes, vec![0x00, 0x00, 0x80, 0x3F]);
    }

    #[test]
    fn decode_embedding_reads_little_endian_layout() {
        let vector = decode_embedding(Some(vec![0x00, 0x00, 0x80, 0x3F]))
            .expect("decode succeeds")
            .expect("payload present");
        assert_eq!(vector, vec![1.0_f32]);
    }

    #[test]
    fn round_trip_preserves_values() {
        let original = vec![-1.5_f32, 0.0, 1.5, f32::MIN_POSITIVE, 42.0];
        let blob = encode_embedding(&original).expect("encode round-trip");
        let decoded = decode_embedding(Some(blob))
            .expect("decode round-trip")
            .expect("payload present");
        assert_eq!(decoded, original);
    }
}
