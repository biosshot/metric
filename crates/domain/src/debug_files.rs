//! Compact, backend-independent identities for native debug files.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use thiserror::Error;

use crate::{OrganizationId, ProjectId, Timestamp};

pub const MAX_DEBUG_FILE_NAME_BYTES: usize = 255;
pub const MAX_DEBUG_CHUNKS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DebugFileValueError {
    #[error("debug identifier is invalid")]
    InvalidDebugId,
    #[error("code identifier is invalid")]
    InvalidCodeId,
    #[error("debug file identifier is invalid")]
    InvalidFileId,
    #[error("debug file name is invalid")]
    InvalidName,
    #[error("debug upload manifest is invalid")]
    InvalidManifest,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DebugId {
    Uuid { uuid: [u8; 16], appendix: u32 },
    Pdb20 { timestamp: u32, age: u32 },
}

impl DebugId {
    pub fn parse(value: &str) -> Result<Self, DebugFileValueError> {
        if let Some((timestamp, age)) = value.split_once(':') {
            if timestamp.len() == 8
                && age.len() <= 8
                && timestamp.bytes().all(|byte| byte.is_ascii_hexdigit())
                && age.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Ok(Self::Pdb20 {
                    timestamp: u32::from_str_radix(timestamp, 16)
                        .map_err(|_| DebugFileValueError::InvalidDebugId)?,
                    age: u32::from_str_radix(age, 16)
                        .map_err(|_| DebugFileValueError::InvalidDebugId)?,
                });
            }
        }
        let (uuid_text, appendix) = if value.len() > 36 {
            let (uuid, appendix) = value
                .split_once('-')
                .and_then(|_| value.rsplit_once('-'))
                .ok_or(DebugFileValueError::InvalidDebugId)?;
            if uuid.len() != 36 || appendix.is_empty() || appendix.len() > 8 {
                return Err(DebugFileValueError::InvalidDebugId);
            }
            (
                uuid,
                u32::from_str_radix(appendix, 16)
                    .map_err(|_| DebugFileValueError::InvalidDebugId)?,
            )
        } else {
            (value, 0)
        };
        let compact = uuid_text.replace('-', "");
        if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DebugFileValueError::InvalidDebugId);
        }
        let mut uuid = [0_u8; 16];
        hex::decode_to_slice(compact, &mut uuid)
            .map_err(|_| DebugFileValueError::InvalidDebugId)?;
        Ok(Self::Uuid { uuid, appendix })
    }

    #[must_use]
    pub fn encode(&self) -> Box<[u8]> {
        match self {
            Self::Uuid { uuid, appendix: 0 } => uuid.to_vec().into_boxed_slice(),
            Self::Uuid { uuid, appendix } => {
                let mut bytes = Vec::with_capacity(20);
                bytes.extend_from_slice(uuid);
                bytes.extend_from_slice(&appendix.to_be_bytes());
                bytes.into_boxed_slice()
            }
            Self::Pdb20 { timestamp, age } => {
                let mut bytes = Vec::with_capacity(8);
                bytes.extend_from_slice(&timestamp.to_be_bytes());
                bytes.extend_from_slice(&age.to_be_bytes());
                bytes.into_boxed_slice()
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DebugFileValueError> {
        match bytes.len() {
            16 => {
                let mut uuid = [0_u8; 16];
                uuid.copy_from_slice(bytes);
                Ok(Self::Uuid { uuid, appendix: 0 })
            }
            20 => {
                let mut uuid = [0_u8; 16];
                uuid.copy_from_slice(&bytes[..16]);
                let appendix =
                    u32::from_be_bytes(bytes[16..].try_into().expect("twenty-byte DebugId suffix"));
                (appendix != 0)
                    .then_some(Self::Uuid { uuid, appendix })
                    .ok_or(DebugFileValueError::InvalidDebugId)
            }
            8 => Ok(Self::Pdb20 {
                timestamp: u32::from_be_bytes(bytes[..4].try_into().expect("PDB timestamp")),
                age: u32::from_be_bytes(bytes[4..].try_into().expect("PDB age")),
            }),
            _ => Err(DebugFileValueError::InvalidDebugId),
        }
    }
}

impl fmt::Display for DebugId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uuid { uuid, appendix } => {
                let encoded = hex::encode(uuid);
                write!(
                    formatter,
                    "{}-{}-{}-{}-{}",
                    &encoded[..8],
                    &encoded[8..12],
                    &encoded[12..16],
                    &encoded[16..20],
                    &encoded[20..]
                )?;
                if *appendix != 0 {
                    write!(formatter, "-{appendix:x}")?;
                }
                Ok(())
            }
            Self::Pdb20 { timestamp, age } => write!(formatter, "{timestamp:08x}:{age:x}"),
        }
    }
}

impl fmt::Debug for DebugId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodeId(Box<str>);

impl CodeId {
    pub fn parse(value: &str) -> Result<Self, DebugFileValueError> {
        let normalized = value.to_ascii_lowercase();
        let valid = !normalized.is_empty()
            && normalized.len() <= 128
            && normalized.bytes().all(|byte| byte.is_ascii_hexdigit());
        valid
            .then(|| Self(normalized.into_boxed_str()))
            .ok_or(DebugFileValueError::InvalidCodeId)
    }

    #[must_use]
    pub fn encode(&self) -> Box<[u8]> {
        let odd = self.0.len() % 2 == 1;
        let mut encoded = Vec::with_capacity(1 + self.0.len().div_ceil(2));
        encoded.push(u8::from(odd));
        let mut digits = self.0.bytes();
        while let Some(high) = digits.next() {
            let high = nibble(high);
            let low = digits.next().map_or(0, nibble);
            encoded.push((high << 4) | low);
        }
        encoded.into_boxed_slice()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DebugFileValueError> {
        let (&flags, packed) = bytes
            .split_first()
            .ok_or(DebugFileValueError::InvalidCodeId)?;
        if flags > 1 || packed.is_empty() || (flags == 1 && packed.last().unwrap() & 0x0f != 0) {
            return Err(DebugFileValueError::InvalidCodeId);
        }
        let mut value = String::with_capacity(packed.len() * 2);
        for (index, byte) in packed.iter().copied().enumerate() {
            value.push(hex_digit(byte >> 4));
            if !(flags == 1 && index + 1 == packed.len()) {
                value.push(hex_digit(byte & 0x0f));
            }
        }
        Self::parse(&value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("CodeId").field(&self.0).finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DebugFileId([u8; 16]);

impl DebugFileId {
    #[must_use]
    pub fn derive(project_id: ProjectId, checksum: [u8; 32]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"faultkeep/debug-file-id/v1");
        hasher.update(&project_id.get().to_be_bytes());
        hasher.update(&checksum);
        let mut id = [0_u8; 16];
        id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        Self(id)
    }

    pub fn parse(value: &str) -> Result<Self, DebugFileValueError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| DebugFileValueError::InvalidFileId)?;
        let id = bytes
            .try_into()
            .map_err(|_| DebugFileValueError::InvalidFileId)?;
        Ok(Self(id))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for DebugFileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for DebugFileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DebugFileType {
    Elf = 0,
    MachO = 1,
    Pe = 2,
    Pdb = 3,
    PortablePdb = 4,
    Breakpad = 5,
}

impl DebugFileType {
    pub fn from_name(name: &str) -> Result<Self, DebugFileValueError> {
        let lowercase = name.to_ascii_lowercase();
        if lowercase.ends_with(".sym") {
            Ok(Self::Breakpad)
        } else if lowercase.ends_with(".pdb") {
            Ok(Self::Pdb)
        } else if lowercase.ends_with(".dll") || lowercase.ends_with(".exe") {
            Ok(Self::Pe)
        } else if lowercase.ends_with(".dylib") || lowercase.ends_with(".dsym") {
            Ok(Self::MachO)
        } else if lowercase.ends_with(".so") || !lowercase.contains('.') {
            Ok(Self::Elf)
        } else {
            Err(DebugFileValueError::InvalidName)
        }
    }

    #[must_use]
    pub const fn symbolicator_name(self) -> &'static str {
        match self {
            Self::Elf => "elf",
            Self::MachO => "macho",
            Self::Pe => "pe",
            Self::Pdb => "pdb",
            Self::PortablePdb => "portablepdb",
            Self::Breakpad => "breakpad",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugFile {
    pub id: DebugFileId,
    pub project_id: ProjectId,
    pub debug_id: Option<DebugId>,
    pub code_id: Option<CodeId>,
    pub file_type: DebugFileType,
    pub checksum: [u8; 32],
    pub sha1: [u8; 20],
    pub size: u64,
    pub name: Box<str>,
    pub uploaded_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugUpload {
    pub id: [u8; 16],
    pub project_id: ProjectId,
    pub organization_id: OrganizationId,
    pub sha1: [u8; 20],
    pub name: Box<str>,
    pub debug_id: Option<DebugId>,
    pub code_id: Option<CodeId>,
    pub chunks: Vec<[u8; 20]>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugUploadState {
    Pending,
    Assembling,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugUploadRecord {
    pub upload: DebugUpload,
    pub state: DebugUploadState,
    pub attempts: u32,
    pub error_code: Option<Box<str>>,
}

pub fn validate_debug_name(value: &str) -> Result<Box<str>, DebugFileValueError> {
    let leaf = value.rsplit(['/', '\\']).next().unwrap_or_default();
    let valid = !leaf.is_empty()
        && leaf.len() <= MAX_DEBUG_FILE_NAME_BYTES
        && !leaf.chars().any(char::is_control);
    valid
        .then(|| leaf.into())
        .ok_or(DebugFileValueError::InvalidName)
}

fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("CodeId is validated before packing"),
    }
}

fn hex_digit(value: u8) -> char {
    char::from(if value < 10 {
        b'0' + value
    } else {
        b'a' + value - 10
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_id_code_id_and_file_id_codecs_are_canonical() {
        for text in [
            "67e9247c-814e-392b-a027-dbde6748fcbf",
            "67e9247c-814e-392b-a027-dbde6748fcbf-a",
            "65a1bc02:3",
        ] {
            let value = DebugId::parse(text).unwrap();
            assert_eq!(DebugId::decode(&value.encode()).unwrap(), value);
        }
        for text in ["a", "01", "abcdef1", "0011223344556677"] {
            let value = CodeId::parse(text).unwrap();
            assert_eq!(CodeId::decode(&value.encode()).unwrap(), value);
        }
        let file = DebugFileId::derive(ProjectId::new(7).unwrap(), [9; 32]);
        assert_eq!(DebugFileId::parse(&file.to_string()).unwrap(), file);
    }

    #[test]
    fn malformed_binary_corpus_is_rejected() {
        assert!(DebugId::decode(&[]).is_err());
        assert!(DebugId::decode(&[0; 20]).is_err());
        assert!(CodeId::decode(&[]).is_err());
        assert!(CodeId::decode(&[2, 0]).is_err());
        assert!(CodeId::decode(&[1, 0x12]).is_err());
        assert!(DebugFileId::parse("not-base64").is_err());
    }
}
