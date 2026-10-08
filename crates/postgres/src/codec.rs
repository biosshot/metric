//! Adapter-owned immutable version 1 canonical JSON, optionally compressed.
use crate::PostgresError;
use serde_json::Value;

pub(crate) fn encode_json(
    value: &Value,
    level: i32,
    minimum_savings: usize,
    maximum: usize,
) -> Result<(Vec<u8>, Vec<u8>), PostgresError> {
    let canonical = serde_json::to_vec(value).map_err(|_| PostgresError::InvalidData)?;
    if canonical.len() > maximum {
        return Err(PostgresError::InvalidData);
    }
    let compressed =
        zstd::bulk::compress(&canonical, level).map_err(|_| PostgresError::InvalidData)?;
    let use_compressed = compressed.len().saturating_add(minimum_savings) <= canonical.len();
    let bytes = if use_compressed {
        &compressed
    } else {
        &canonical
    };
    let mut encoded = Vec::with_capacity(bytes.len() + 2);
    encoded.extend_from_slice(&[1, u8::from(use_compressed)]);
    encoded.extend_from_slice(bytes);
    Ok((encoded, canonical))
}

pub(crate) fn decode_json(body: &[u8], maximum: usize) -> Result<Vec<u8>, PostgresError> {
    if body.first() != Some(&1) || body.len() < 2 {
        return Err(PostgresError::InvalidData);
    }
    let bytes = match body[1] {
        0 if body.len() - 2 <= maximum => body[2..].to_vec(),
        1 => zstd::bulk::decompress(&body[2..], maximum).map_err(|_| PostgresError::InvalidData)?,
        _ => return Err(PostgresError::InvalidData),
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| PostgresError::InvalidData)?;
    if serde_json::to_vec(&value).map_err(|_| PostgresError::InvalidData)? != bytes {
        return Err(PostgresError::InvalidData);
    }
    Ok(bytes)
}

pub(crate) fn decode_utf8(bytes: Vec<u8>) -> Result<String, PostgresError> {
    String::from_utf8(bytes).map_err(|_| PostgresError::InvalidData)
}
