//! Independent bounded reader for Incident Capsule compatibility tests.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};

use serde::Deserialize;
use thiserror::Error;

const MAX_FILES: usize = 17;
const MAX_ARCHIVE_BYTES: usize = 110 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapsuleReaderError {
    #[error("archive is malformed")]
    Malformed,
    #[error("archive format version is unsupported")]
    UnsupportedVersion,
    #[error("archive path is unsafe or duplicated")]
    UnsafePath,
    #[error("archive exceeds a reader limit")]
    LimitExceeded,
    #[error("archive manifest is invalid")]
    InvalidManifest,
    #[error("archive entry integrity check failed")]
    Integrity,
}

#[derive(Debug)]
pub struct ValidatedCapsule {
    pub manifest: serde_json::Value,
    pub entries: BTreeMap<String, Vec<u8>>,
}

#[derive(Deserialize)]
struct Manifest {
    format: String,
    version: u16,
    entries: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    path: String,
    uncompressed_size: u64,
    blake3: String,
}

pub fn validate(bytes: &[u8]) -> Result<ValidatedCapsule, CapsuleReaderError> {
    if bytes.is_empty() || bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(CapsuleReaderError::LimitExceeded);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| CapsuleReaderError::Malformed)?;
    if archive.is_empty() || archive.len() > MAX_FILES {
        return Err(CapsuleReaderError::LimitExceeded);
    }
    let mut paths = BTreeSet::new();
    let mut entries = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|_| CapsuleReaderError::Malformed)?;
        let path = file.name().to_owned();
        if !safe_path(&path)
            || !allowed_path(&path)
            || !paths.insert(path.clone())
            || file.is_dir()
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(CapsuleReaderError::UnsafePath);
        }
        if file.size() > MAX_ENTRY_BYTES {
            return Err(CapsuleReaderError::LimitExceeded);
        }
        if file.compressed_size() > 0
            && file.size() / file.compressed_size() > MAX_COMPRESSION_RATIO
        {
            return Err(CapsuleReaderError::LimitExceeded);
        }
        total = total
            .checked_add(file.size())
            .ok_or(CapsuleReaderError::LimitExceeded)?;
        if total > MAX_TOTAL_BYTES {
            return Err(CapsuleReaderError::LimitExceeded);
        }
        let expected_size = file.size();
        let mut content = Vec::with_capacity(
            usize::try_from(expected_size).map_err(|_| CapsuleReaderError::LimitExceeded)?,
        );
        file.by_ref()
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut content)
            .map_err(|_| CapsuleReaderError::Malformed)?;
        if content.len() as u64 != expected_size {
            return Err(CapsuleReaderError::Malformed);
        }
        entries.insert(path, content);
    }

    let manifest_bytes = entries
        .get("manifest.json")
        .ok_or(CapsuleReaderError::InvalidManifest)?;
    let manifest_value: serde_json::Value =
        serde_json::from_slice(manifest_bytes).map_err(|_| CapsuleReaderError::InvalidManifest)?;
    let manifest: Manifest = serde_json::from_value(manifest_value.clone())
        .map_err(|_| CapsuleReaderError::InvalidManifest)?;
    if manifest.format != "incident-capsule" {
        return Err(CapsuleReaderError::InvalidManifest);
    }
    if manifest.version != 1 {
        return Err(CapsuleReaderError::UnsupportedVersion);
    }
    let mut declared = BTreeSet::new();
    for expected in &manifest.entries {
        if expected.path == "manifest.json"
            || !declared.insert(expected.path.as_str())
            || !allowed_path(&expected.path)
        {
            return Err(CapsuleReaderError::InvalidManifest);
        }
        let content = entries
            .get(&expected.path)
            .ok_or(CapsuleReaderError::Integrity)?;
        if content.len() as u64 != expected.uncompressed_size
            || blake3::hash(content).to_hex().as_str() != expected.blake3
        {
            return Err(CapsuleReaderError::Integrity);
        }
    }
    let actual = entries
        .keys()
        .filter(|path| path.as_str() != "manifest.json")
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if actual != declared {
        return Err(CapsuleReaderError::InvalidManifest);
    }
    Ok(ValidatedCapsule {
        manifest: manifest_value,
        entries,
    })
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 128
        && path.is_ascii()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !path.bytes().any(|byte| byte.is_ascii_control())
}

fn allowed_path(path: &str) -> bool {
    matches!(
        path,
        "manifest.json"
            | "issue.json"
            | "statistics/hourly.json"
            | "activity.json"
            | "diagnostics/capabilities.json"
            | "README.txt"
    ) || path
        .strip_prefix("events/")
        .and_then(|value| value.strip_suffix(".json"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    #[test]
    fn reader_rejects_traversal_duplicate_and_archive_bomb_metadata() {
        assert!(matches!(
            validate(&archive(&[
                ("../issue.json", b"{}"),
                ("manifest.json", b"{}")
            ])),
            Err(CapsuleReaderError::UnsafePath)
        ));
        assert!(validate(&duplicate_name_archive()).is_err());
        let compressed = vec![b'a'; 2 * 1024 * 1024];
        assert!(matches!(
            validate(&archive(&[
                ("issue.json", compressed.as_slice()),
                ("manifest.json", b"{}"),
            ])),
            Err(CapsuleReaderError::LimitExceeded)
        ));
    }

    #[test]
    fn reader_accepts_unknown_manifest_fields_and_rejects_integrity_version_and_truncation() {
        let valid = valid_archive(1, blake3::hash(b"{}").to_hex().as_str());
        let capsule = validate(&valid).unwrap();
        assert_eq!(capsule.manifest["future_optional"], "ignored");

        let corrupt = valid_archive(1, &"0".repeat(64));
        assert!(matches!(
            validate(&corrupt),
            Err(CapsuleReaderError::Integrity)
        ));
        let unsupported = valid_archive(2, blake3::hash(b"{}").to_hex().as_str());
        assert!(matches!(
            validate(&unsupported),
            Err(CapsuleReaderError::UnsupportedVersion)
        ));
        assert!(matches!(
            validate(&valid[..valid.len() - 7]),
            Err(CapsuleReaderError::Malformed)
        ));
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (path, bytes) in entries {
            writer.start_file(*path, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn duplicate_name_archive() -> Vec<u8> {
        let mut bytes = archive(&[("manifest.json", b"{}"), ("activity.json", b"{}")]);
        for offset in 0..=bytes.len() - b"activity.json".len() {
            if bytes[offset..].starts_with(b"activity.json") {
                bytes[offset..offset + b"activity.json".len()].copy_from_slice(b"manifest.json");
            }
        }
        bytes
    }

    fn valid_archive(version: u16, checksum: &str) -> Vec<u8> {
        let manifest = serde_json::to_vec(&serde_json::json!({
            "format": "incident-capsule",
            "version": version,
            "generated_at": "2026-07-21T00:00:00Z",
            "organization_id": "7",
            "project_id": "42",
            "issue_id": "00112233445566778899aabbccddeeff",
            "selection": {"mode": "default", "event_ids": []},
            "entries": [{
                "path": "issue.json",
                "media_type": "application/json",
                "uncompressed_size": 2,
                "blake3": checksum,
            }],
            "omissions": [],
            "future_optional": "ignored",
        }))
        .unwrap();
        archive(&[("issue.json", b"{}"), ("manifest.json", &manifest)])
    }
}
