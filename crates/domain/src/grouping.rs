//! Pure versioned deterministic Event grouping from ADR-0014.

use std::fmt;

use thiserror::Error;

use crate::{
    ProjectId,
    event::{CanonicalValue, NormalizedEventBody, NormalizedFrame},
    symbolication::{RawTraceOrigin, SymbolicatedFrame, SymbolicationResult, SymbolicationStatus},
};

const REVISION_1: u16 = 1;
const MAX_COMPONENTS: usize = 64;
const MAX_COMPONENT_BYTES: usize = 2 * 1024;
const MAX_GROUPING_FRAMES: usize = 8;
const MAX_EXCEPTION_TYPES: usize = 8;
const DEFAULT_FINGERPRINT: &str = "{{ default }}";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupingKey {
    revision: u16,
    digest: [u8; 32],
}

impl GroupingKey {
    const fn from_digest(revision: u16, digest: [u8; 32]) -> Self {
        Self { revision, digest }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, GroupingKeyError> {
        let bytes: &[u8; 34] = bytes
            .try_into()
            .map_err(|_| GroupingKeyError::InvalidLength)?;
        let revision = u16::from_be_bytes([bytes[0], bytes[1]]);
        if revision == 0 {
            return Err(GroupingKeyError::InvalidRevision);
        }
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(&bytes[2..]);
        Ok(Self { revision, digest })
    }

    #[must_use]
    pub const fn revision(self) -> u16 {
        self.revision
    }

    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; 34] {
        let mut bytes = [0_u8; 34];
        bytes[..2].copy_from_slice(&self.revision.to_be_bytes());
        bytes[2..].copy_from_slice(&self.digest);
        bytes
    }
}

impl fmt::Debug for GroupingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GroupingKey(v{}:{})",
            self.revision,
            hex::encode(self.digest)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupingKeyError {
    #[error("GroupingKey must contain exactly 34 bytes")]
    InvalidLength,
    #[error("GroupingKey revision must be nonzero")]
    InvalidRevision,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IssueId([u8; 16]);

impl IssueId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for IssueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "IssueId({})", hex::encode(self.0))
    }
}

impl fmt::Display for IssueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupingStrategy {
    SdkFingerprint,
    ExceptionStack,
    NativeStack,
    Message,
}

impl GroupingStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SdkFingerprint => "sdk_fingerprint",
            Self::ExceptionStack => "exception_stack",
            Self::NativeStack => "native_stack",
            Self::Message => "message",
        }
    }

    const fn domain(self) -> &'static [u8] {
        match self {
            Self::SdkFingerprint => b"sdk_fingerprint/v1",
            Self::ExceptionStack => b"exception_stack/v1",
            Self::NativeStack => b"native_stack/v1",
            Self::Message => b"message/v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GroupingComponentKind {
    SdkFingerprint = 1,
    DefaultStrategy = 2,
    DefaultDigest = 3,
    ExceptionType = 4,
    Frame = 5,
    FrameFunction = 6,
    FrameModule = 7,
    FramePath = 8,
    FrameLine = 9,
    NativeModule = 10,
    NativeRelativeAddress = 11,
    Logger = 12,
    Message = 13,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupingComponent {
    pub kind: GroupingComponentKind,
    pub value: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupingExplanation {
    pub summary: Box<str>,
    pub components: Vec<GroupingComponent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupingResult {
    pub key: GroupingKey,
    pub issue_id: IssueId,
    pub strategy: GroupingStrategy,
    pub explanation: GroupingExplanation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupingError {
    #[error("grouping revision is unsupported")]
    UnsupportedRevision,
    #[error("grouping input exceeds the revision-owned component bounds")]
    InputLimitExceeded,
}

#[must_use]
pub const fn supported_revisions() -> &'static [u16] {
    &[REVISION_1]
}

pub fn group(
    project_id: ProjectId,
    pinned_revision: u64,
    body: &NormalizedEventBody,
    symbolication: Option<&SymbolicationResult>,
) -> Result<GroupingResult, GroupingError> {
    let revision =
        u16::try_from(pinned_revision).map_err(|_| GroupingError::UnsupportedRevision)?;
    if revision != REVISION_1 {
        return Err(GroupingError::UnsupportedRevision);
    }
    group_revision_1(project_id, body, symbolication)
}

#[must_use]
pub fn derive_issue_id(project_id: ProjectId, key: GroupingKey) -> IssueId {
    let mut encoding = Encoder::default();
    encoding.bytes(b"issue-id/v1");
    encoding.bytes(&project_id.get().to_be_bytes());
    encoding.bytes(&key.to_bytes());
    let digest = blake3::hash(&encoding.bytes);
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    IssueId(id)
}

#[must_use]
pub fn verify_issue_id(project_id: ProjectId, key: GroupingKey, issue_id: IssueId) -> bool {
    derive_issue_id(project_id, key) == issue_id
}

fn group_revision_1(
    project_id: ProjectId,
    body: &NormalizedEventBody,
    symbolication: Option<&SymbolicationResult>,
) -> Result<GroupingResult, GroupingError> {
    let selected = if body.fingerprint.is_empty() {
        select_default(body, symbolication)?
    } else {
        select_fingerprint(body, symbolication)?
    };
    validate_components(&selected.components)?;
    let key = key_for(REVISION_1, selected.strategy, &selected.components);
    let issue_id = derive_issue_id(project_id, key);
    Ok(GroupingResult {
        key,
        issue_id,
        strategy: selected.strategy,
        explanation: GroupingExplanation {
            summary: selected.summary,
            components: selected.components,
        },
    })
}

struct Selected {
    strategy: GroupingStrategy,
    components: Vec<GroupingComponent>,
    summary: Box<str>,
}

fn select_fingerprint(
    body: &NormalizedEventBody,
    symbolication: Option<&SymbolicationResult>,
) -> Result<Selected, GroupingError> {
    let needs_default = body
        .fingerprint
        .iter()
        .any(|value| value.as_ref() == DEFAULT_FINGERPRINT);
    let default = needs_default
        .then(|| select_default(body, symbolication))
        .transpose()?;
    let default_key = default
        .as_ref()
        .map(|selected| key_for(REVISION_1, selected.strategy, &selected.components));
    let mut components = Vec::with_capacity(body.fingerprint.len().saturating_add(2));
    for value in &body.fingerprint {
        if value.as_ref() == DEFAULT_FINGERPRINT {
            let selected = default.as_ref().expect("default selection was requested");
            let key = default_key.expect("default key was requested");
            push_component(
                &mut components,
                GroupingComponentKind::DefaultStrategy,
                selected.strategy.as_str(),
            );
            push_component(
                &mut components,
                GroupingComponentKind::DefaultDigest,
                &hex::encode(key.digest()),
            );
        } else {
            push_component(
                &mut components,
                GroupingComponentKind::SdkFingerprint,
                value,
            );
        }
    }
    Ok(Selected {
        strategy: GroupingStrategy::SdkFingerprint,
        summary: format!(
            "SDK fingerprint with {} component(s)",
            body.fingerprint.len()
        )
        .into(),
        components,
    })
}

fn select_default(
    body: &NormalizedEventBody,
    symbolication: Option<&SymbolicationResult>,
) -> Result<Selected, GroupingError> {
    if let Some(selected) = exception_stack(body, symbolication) {
        return Ok(selected);
    }
    if let Some(selected) = native_stack(body) {
        return Ok(selected);
    }
    Ok(message_strategy(body))
}

fn exception_stack(
    body: &NormalizedEventBody,
    symbolication: Option<&SymbolicationResult>,
) -> Option<Selected> {
    if body
        .exceptions
        .iter()
        .flat_map(|exception| &exception.stacktrace)
        .chain(&body.stacktrace)
        .any(|frame| frame.instruction_address.is_some() || frame.symbol_address.is_some())
    {
        return None;
    }
    let mut types = body
        .exceptions
        .iter()
        .rev()
        .filter_map(|exception| exception.ty.as_deref())
        .take(MAX_EXCEPTION_TYPES)
        .collect::<Vec<_>>();
    types.reverse();

    for (index, exception) in body.exceptions.iter().enumerate().rev() {
        let origin = RawTraceOrigin::Exception { index };
        let derived = derived_frames(symbolication, origin);
        let frames = if let Some(derived) = derived {
            significant_derived_frames(derived)
        } else {
            significant_raw_frames(&exception.stacktrace)
        };
        if frames.is_empty() {
            continue;
        }
        let mut components = Vec::new();
        for ty in &types {
            push_component(&mut components, GroupingComponentKind::ExceptionType, ty);
        }
        append_frame_components(&mut components, &frames);
        return Some(Selected {
            strategy: GroupingStrategy::ExceptionStack,
            summary: format!(
                "exception stack with {} type(s) and {} significant frame(s)",
                types.len(),
                frames.len()
            )
            .into(),
            components,
        });
    }

    let frames = if let Some(derived) = derived_frames(symbolication, RawTraceOrigin::Event) {
        significant_derived_frames(derived)
    } else {
        significant_raw_frames(&body.stacktrace)
    };
    if frames.is_empty() {
        return None;
    }
    let mut components = Vec::new();
    for ty in &types {
        push_component(&mut components, GroupingComponentKind::ExceptionType, ty);
    }
    append_frame_components(&mut components, &frames);
    Some(Selected {
        strategy: GroupingStrategy::ExceptionStack,
        summary: format!("event stack with {} significant frame(s)", frames.len()).into(),
        components,
    })
}

enum FrameIdentity {
    Raw {
        function: Option<Box<str>>,
        module: Option<Box<str>>,
        path: Option<Box<str>>,
        line: Option<u64>,
    },
    Derived {
        function: Option<Box<str>>,
        module: Option<Box<str>>,
        path: Option<Box<str>>,
        line: Option<u64>,
    },
}

impl FrameIdentity {
    fn fields(&self) -> (Option<&str>, Option<&str>, Option<&str>, Option<u64>) {
        match self {
            Self::Raw {
                function,
                module,
                path,
                line,
            }
            | Self::Derived {
                function,
                module,
                path,
                line,
            } => (
                function.as_deref(),
                module.as_deref(),
                path.as_deref(),
                *line,
            ),
        }
    }
}

fn significant_raw_frames(frames: &[NormalizedFrame]) -> Vec<FrameIdentity> {
    frames
        .iter()
        .rev()
        .filter(|frame| {
            frame.instruction_address.is_none()
                && frame.symbol_address.is_none()
                && frame.in_app != Some(false)
                && (frame.function.is_some()
                    || frame.module.is_some()
                    || frame.filename.is_some()
                    || frame.absolute_path.is_some())
        })
        .take(MAX_GROUPING_FRAMES)
        .map(|frame| FrameIdentity::Raw {
            function: frame.function.as_deref().map(normalize_identifier),
            module: frame.module.as_deref().map(normalize_identifier),
            path: frame
                .filename
                .as_deref()
                .or(frame.absolute_path.as_deref())
                .map(normalize_path),
            line: frame.function.is_none().then_some(frame.line).flatten(),
        })
        .collect()
}

fn significant_derived_frames(frames: &[SymbolicatedFrame]) -> Vec<FrameIdentity> {
    frames
        .iter()
        .rev()
        .filter(|frame| {
            frame.function.is_some() || frame.module.is_some() || frame.filename.is_some()
        })
        .take(MAX_GROUPING_FRAMES)
        .map(|frame| FrameIdentity::Derived {
            function: frame.function.as_deref().map(normalize_identifier),
            module: frame.module.as_deref().map(normalize_identifier),
            path: frame.filename.as_deref().map(normalize_path),
            line: frame.function.is_none().then_some(frame.line).flatten(),
        })
        .collect()
}

fn append_frame_components(components: &mut Vec<GroupingComponent>, frames: &[FrameIdentity]) {
    for (index, frame) in frames.iter().enumerate() {
        push_component(components, GroupingComponentKind::Frame, &index.to_string());
        let (function, module, path, line) = frame.fields();
        if let Some(function) = function {
            push_component(components, GroupingComponentKind::FrameFunction, function);
        }
        if let Some(module) = module {
            push_component(components, GroupingComponentKind::FrameModule, module);
        }
        if let Some(path) = path {
            push_component(components, GroupingComponentKind::FramePath, path);
        }
        if let Some(line) = line {
            push_component(
                components,
                GroupingComponentKind::FrameLine,
                &line.to_string(),
            );
        }
    }
}

fn native_stack(body: &NormalizedEventBody) -> Option<Selected> {
    let modules = native_modules(body);
    if modules.is_empty() {
        return None;
    }
    let mut pairs = Vec::new();
    for frame in body
        .exceptions
        .iter()
        .flat_map(|exception| &exception.stacktrace)
        .chain(&body.stacktrace)
        .rev()
    {
        let Some(address) = frame
            .instruction_address
            .as_deref()
            .and_then(parse_hex_address)
        else {
            continue;
        };
        if let Some(module) = modules.iter().rev().find(|module| module.contains(address)) {
            pairs.push((module.identity.as_ref(), address - module.base));
            if pairs.len() == MAX_GROUPING_FRAMES {
                break;
            }
        }
    }
    if pairs.is_empty() {
        return None;
    }
    let mut components = Vec::new();
    if let Some(exception_type) = body
        .exceptions
        .iter()
        .rev()
        .find_map(|value| value.ty.as_deref())
    {
        push_component(
            &mut components,
            GroupingComponentKind::ExceptionType,
            exception_type,
        );
    }
    for (module, relative) in &pairs {
        push_component(&mut components, GroupingComponentKind::NativeModule, module);
        push_component(
            &mut components,
            GroupingComponentKind::NativeRelativeAddress,
            &format!("{relative:x}"),
        );
    }
    Some(Selected {
        strategy: GroupingStrategy::NativeStack,
        summary: format!("native stack with {} module-relative frame(s)", pairs.len()).into(),
        components,
    })
}

struct NativeModule {
    identity: Box<str>,
    base: u64,
    size: Option<u64>,
}

impl NativeModule {
    fn contains(&self, address: u64) -> bool {
        address >= self.base
            && self
                .size
                .and_then(|size| self.base.checked_add(size))
                .is_none_or(|end| address < end)
    }
}

fn native_modules(body: &NormalizedEventBody) -> Vec<NativeModule> {
    let Some(CanonicalValue::Object(debug_meta)) = body.unknown.get("debug_meta") else {
        return Vec::new();
    };
    let Some(CanonicalValue::Array(images)) = debug_meta.get("images") else {
        return Vec::new();
    };
    let mut modules = images
        .iter()
        .filter_map(|image| {
            let CanonicalValue::Object(image) = image else {
                return None;
            };
            let identity = canonical_string(image.get("debug_id"))
                .or_else(|| canonical_string(image.get("code_id")))?;
            let base = canonical_string(image.get("image_addr")).and_then(parse_hex_address)?;
            let size = canonical_u64(image.get("image_size"));
            Some(NativeModule {
                identity: normalize_identifier(identity),
                base,
                size,
            })
        })
        .collect::<Vec<_>>();
    modules.sort_by_key(|module| module.base);
    modules
}

fn canonical_string(value: Option<&CanonicalValue>) -> Option<&str> {
    let CanonicalValue::String(value) = value? else {
        return None;
    };
    Some(value)
}

fn canonical_u64(value: Option<&CanonicalValue>) -> Option<u64> {
    let CanonicalValue::Number(value) = value? else {
        return None;
    };
    value.parse().ok()
}

fn parse_hex_address(value: &str) -> Option<u64> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    (!value.is_empty())
        .then(|| u64::from_str_radix(value, 16).ok())
        .flatten()
}

fn message_strategy(body: &NormalizedEventBody) -> Selected {
    let mut components = Vec::new();
    if let Some(logger) = body.logger.as_deref() {
        push_component(
            &mut components,
            GroupingComponentKind::Logger,
            &normalize_identifier(logger),
        );
    }
    let message = body
        .message
        .as_deref()
        .map(normalize_message)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "<empty error>".into());
    push_component(&mut components, GroupingComponentKind::Message, &message);
    Selected {
        strategy: GroupingStrategy::Message,
        summary: "logger and normalized message".into(),
        components,
    }
}

fn derived_frames(
    symbolication: Option<&SymbolicationResult>,
    origin: RawTraceOrigin,
) -> Option<&[SymbolicatedFrame]> {
    let result = symbolication?;
    if !matches!(
        result.status,
        SymbolicationStatus::Complete | SymbolicationStatus::Partial
    ) {
        return None;
    }
    result
        .derived
        .iter()
        .find(|trace| trace.origin == origin)
        .map(|trace| trace.frames.as_slice())
}

fn normalize_identifier(value: &str) -> Box<str> {
    value.trim().to_ascii_lowercase().into()
}

fn normalize_path(value: &str) -> Box<str> {
    let normalized = value.replace('\\', "/");
    let without_query = normalized.split(['?', '#']).next().unwrap_or(&normalized);
    let parts = without_query
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts[parts.len().saturating_sub(2)..]
        .join("/")
        .to_ascii_lowercase()
        .into()
}

fn normalize_message(value: &str) -> Box<str> {
    let normalized = value
        .split_whitespace()
        .map(normalize_message_token)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_utf8(&normalized, MAX_COMPONENT_BYTES).into()
}

fn truncate_utf8(value: &str, maximum: usize) -> String {
    let mut end = maximum.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn normalize_message_token(token: &str) -> String {
    let trimmed = token
        .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '-');
    if is_uuid(trimmed) {
        return "{uuid}".to_owned();
    }
    if is_address(token) {
        return "{address}".to_owned();
    }
    if trimmed.len() >= 6 && trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return "{number}".to_owned();
    }
    if trimmed.len() >= 16
        && trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && trimmed.bytes().any(|byte| byte.is_ascii_alphabetic())
        && trimmed.bytes().any(|byte| byte.is_ascii_digit())
    {
        return "{id}".to_owned();
    }
    token.to_ascii_lowercase()
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_address(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .is_some_and(|digits| {
            digits.len() >= 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn push_component(
    components: &mut Vec<GroupingComponent>,
    kind: GroupingComponentKind,
    value: &str,
) {
    components.push(GroupingComponent {
        kind,
        value: value.into(),
    });
}

fn validate_components(components: &[GroupingComponent]) -> Result<(), GroupingError> {
    if components.is_empty()
        || components.len() > MAX_COMPONENTS
        || components
            .iter()
            .any(|component| component.value.len() > MAX_COMPONENT_BYTES)
    {
        return Err(GroupingError::InputLimitExceeded);
    }
    Ok(())
}

fn key_for(
    revision: u16,
    strategy: GroupingStrategy,
    components: &[GroupingComponent],
) -> GroupingKey {
    let mut encoding = Encoder::default();
    encoding.bytes(strategy.domain());
    encoding.bytes(&revision.to_be_bytes());
    for component in components {
        encoding.byte(component.kind as u8);
        encoding.bytes(component.value.as_bytes());
    }
    GroupingKey::from_digest(revision, *blake3::hash(&encoding.bytes).as_bytes())
}

#[derive(Default)]
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bytes(&mut self, value: &[u8]) {
        let length = u32::try_from(value.len()).expect("revision-owned component bound fits u32");
        self.bytes.extend_from_slice(&length.to_be_bytes());
        self.bytes.extend_from_slice(value);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        Timestamp,
        event::{
            EventLevel, EventPlatform, NormalizedEventBody, NormalizedException, NormalizedTag,
        },
        symbolication::{SymbolicatedStacktrace, SymbolicationDisposition},
    };

    use super::*;

    fn body() -> NormalizedEventBody {
        NormalizedEventBody {
            occurred_at: Timestamp::from_unix_millis(1_753_200_000_000).unwrap(),
            platform: EventPlatform::Python,
            level: EventLevel::Error,
            logger: Some("service".into()),
            message: Some("request failed with HTTP 500".into()),
            transaction: None,
            release: Some("app@1".into()),
            dist: None,
            environment: Some("production".into()),
            fingerprint: Vec::new(),
            exceptions: Vec::new(),
            stacktrace: Vec::new(),
            tags: vec![NormalizedTag {
                key: "user".into(),
                value: "ignored".into(),
            }],
            request: None,
            user: None,
            contexts: BTreeMap::new(),
            breadcrumbs: Vec::new(),
            unknown: BTreeMap::new(),
        }
    }

    fn frame(function: &str, filename: &str) -> NormalizedFrame {
        NormalizedFrame {
            filename: Some(filename.into()),
            absolute_path: None,
            function: Some(function.into()),
            module: Some("app".into()),
            package: None,
            instruction_address: None,
            symbol_address: None,
            line: Some(42),
            column: None,
            in_app: Some(true),
            context_line: None,
            pre_context: Vec::new(),
            post_context: Vec::new(),
            variables: BTreeMap::new(),
            unknown: BTreeMap::new(),
        }
    }

    fn exception(ty: &str, frames: Vec<NormalizedFrame>) -> NormalizedException {
        NormalizedException {
            ty: Some(ty.into()),
            value: None,
            module: None,
            thread_id: None,
            mechanism: None,
            stacktrace: frames,
            raw_stacktrace: Vec::new(),
            unknown: BTreeMap::new(),
        }
    }

    #[test]
    fn revision_registry_and_key_codec_fail_closed() {
        assert_eq!(supported_revisions(), &[1]);
        assert_eq!(
            group(ProjectId::new(1).unwrap(), 0, &body(), None),
            Err(GroupingError::UnsupportedRevision)
        );
        assert_eq!(
            group(ProjectId::new(1).unwrap(), 2, &body(), None),
            Err(GroupingError::UnsupportedRevision)
        );
        assert_eq!(
            GroupingKey::parse(&[0; 33]),
            Err(GroupingKeyError::InvalidLength)
        );
        assert_eq!(
            GroupingKey::parse(&[0; 34]),
            Err(GroupingKeyError::InvalidRevision)
        );
    }

    #[test]
    fn ignored_event_fields_merge_while_semantic_components_separate() {
        let project = ProjectId::new(42).unwrap();
        let first = group(project, 1, &body(), None).unwrap();
        let mut ignored = body();
        ignored.environment = Some("staging".into());
        ignored.release = Some("app@2".into());
        ignored.level = EventLevel::Fatal;
        ignored.occurred_at = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        ignored.tags[0].value = "other".into();
        assert_eq!(first.key, group(project, 1, &ignored, None).unwrap().key);

        let mut distinct = body();
        distinct.message = Some("request failed with HTTP 404".into());
        assert_ne!(first.key, group(project, 1, &distinct, None).unwrap().key);
    }

    #[test]
    fn volatile_message_values_merge_but_short_status_codes_do_not() {
        let project = ProjectId::new(42).unwrap();
        let mut first = body();
        first.message = Some(
            "request 550e8400-e29b-41d4-a716-446655440000 at 0xabcdef1234 failed 123456".into(),
        );
        let mut second = body();
        second.message = Some(
            "request 123e4567-e89b-12d3-a456-426614174000 at 0x1234567890 failed 987654".into(),
        );
        assert_eq!(
            group(project, 1, &first, None).unwrap().key,
            group(project, 1, &second, None).unwrap().key
        );
    }

    #[test]
    fn sdk_fingerprint_priority_and_default_placeholder_are_stable() {
        let project = ProjectId::new(42).unwrap();
        let mut explicit = body();
        explicit.fingerprint = vec!["billing".into(), "timeout".into()];
        let first = group(project, 1, &explicit, None).unwrap();
        explicit.message = Some("completely different".into());
        let second = group(project, 1, &explicit, None).unwrap();
        assert_eq!(first.strategy, GroupingStrategy::SdkFingerprint);
        assert_eq!(first.key, second.key);

        let mut with_default = body();
        with_default.fingerprint = vec![DEFAULT_FINGERPRINT.into(), "tenant".into()];
        let before = group(project, 1, &with_default, None).unwrap();
        with_default.message = Some("different 500".into());
        let after = group(project, 1, &with_default, None).unwrap();
        assert_ne!(before.key, after.key);
    }

    #[test]
    fn exception_stack_is_ordered_newest_first_and_path_roots_are_removed() {
        let project = ProjectId::new(42).unwrap();
        let mut first = body();
        first.exceptions = vec![exception(
            "RuntimeError",
            vec![
                frame("outer", "/srv/build/src/outer.py"),
                frame("inner", "/srv/build/src/inner.py"),
            ],
        )];
        let grouped = group(project, 1, &first, None).unwrap();
        assert_eq!(grouped.strategy, GroupingStrategy::ExceptionStack);
        assert!(
            grouped
                .explanation
                .components
                .iter()
                .any(|component| component.value.as_ref() == "src/inner.py")
        );

        let mut another_root = first.clone();
        another_root.exceptions[0].stacktrace[0].filename = Some("C:\\agent\\src\\outer.py".into());
        another_root.exceptions[0].stacktrace[1].filename = Some("C:\\agent\\src\\inner.py".into());
        assert_eq!(
            grouped.key,
            group(project, 1, &another_root, None).unwrap().key
        );
    }

    #[test]
    fn issue_identity_uses_project_and_complete_key() {
        let first = group(ProjectId::new(42).unwrap(), 1, &body(), None).unwrap();
        let other_project = derive_issue_id(ProjectId::new(43).unwrap(), first.key);
        assert_ne!(first.issue_id, other_project);
        assert!(verify_issue_id(
            ProjectId::new(42).unwrap(),
            first.key,
            first.issue_id
        ));
        assert!(!verify_issue_id(
            ProjectId::new(42).unwrap(),
            first.key,
            IssueId::from_bytes([7; 16])
        ));
        assert_eq!(
            GroupingKey::parse(&first.key.to_bytes()).unwrap(),
            first.key
        );
    }

    fn native_body(debug_id: &str, address: &str) -> NormalizedEventBody {
        let mut body = body();
        body.platform = EventPlatform::Native;
        let mut native_frame = frame("raw_name", "raw.cpp");
        native_frame.instruction_address = Some(address.into());
        body.stacktrace = vec![native_frame];
        body.unknown.insert(
            "debug_meta".into(),
            CanonicalValue::Object(BTreeMap::from([(
                "images".into(),
                CanonicalValue::Array(vec![CanonicalValue::Object(BTreeMap::from([
                    ("debug_id".into(), CanonicalValue::String(debug_id.into())),
                    ("image_addr".into(), CanonicalValue::String("0x1000".into())),
                    ("image_size".into(), CanonicalValue::Number("4096".into())),
                ]))]),
            )])),
        );
        body
    }

    #[test]
    fn native_key_ignores_derived_symbols_but_separates_module_or_relative_address() {
        let project = ProjectId::new(42).unwrap();
        let body = native_body("DEBUG-A", "0x1010");
        let raw = vec![crate::symbolication::RawStacktrace {
            origin: RawTraceOrigin::Event,
            frames: body.stacktrace.clone(),
        }];
        let symbolication = |function: &str| SymbolicationResult {
            kind: crate::symbolication::SymbolicationKind::Native,
            status: SymbolicationStatus::Complete,
            disposition: SymbolicationDisposition::Continue,
            raw: raw.clone(),
            derived: vec![SymbolicatedStacktrace {
                origin: RawTraceOrigin::Event,
                frames: vec![SymbolicatedFrame {
                    original_index: 0,
                    function: Some(function.into()),
                    filename: Some("symbolicated.cpp".into()),
                    module: Some("demo".into()),
                    line: Some(99),
                    column: None,
                }],
            }],
            missing_debug_ids: Vec::new(),
            diagnostics: Vec::new(),
        };
        let first = group(project, 1, &body, Some(&symbolication("one"))).unwrap();
        let second = group(project, 1, &body, Some(&symbolication("two"))).unwrap();
        assert_eq!(first.strategy, GroupingStrategy::NativeStack);
        assert_eq!(first.key, second.key);
        assert_ne!(
            first.key,
            group(project, 1, &native_body("DEBUG-B", "0x1010"), None)
                .unwrap()
                .key
        );
        assert_ne!(
            first.key,
            group(project, 1, &native_body("DEBUG-A", "0x1020"), None)
                .unwrap()
                .key
        );
    }

    #[test]
    fn exact_revision_1_golden_vectors() {
        let project = ProjectId::new(42).unwrap();
        let message = group(project, 1, &body(), None).unwrap();

        let mut exception_body = body();
        exception_body.exceptions = vec![exception(
            "RuntimeError",
            vec![frame("main", "/app/src/main.py")],
        )];
        let exception = group(project, 1, &exception_body, None).unwrap();

        let native = group(project, 1, &native_body("DEBUG-A", "0x1010"), None).unwrap();

        let mut fingerprint_body = body();
        fingerprint_body.fingerprint = vec!["billing".into(), "timeout".into()];
        let fingerprint = group(project, 1, &fingerprint_body, None).unwrap();

        for (grouped, expected_key, expected_issue) in [
            (
                message,
                "00018aa455fd44457238ea7f9592d8555fe92fef9ed2faf4b8af78a5087299603d97",
                "721c66145f03fbdcaaa2164ba850e316",
            ),
            (
                exception,
                "0001cbc968a7ddaee06919d0db32d78d9eff119836a2097cc98a1e759b6bc3efe87d",
                "db33d05dfa3d12f262bfd40f258c6133",
            ),
            (
                native,
                "0001de495c66c2b3ba5b263b8f056de42343db4dbb01d0b09cb39725e76ee03a6a70",
                "140f569e3772a68eb7bf803103e0c8ff",
            ),
            (
                fingerprint,
                "0001298b28fbda9d732427ab08e43cc8e97edcc794e557f8ed1995af822333fa3408",
                "44cdf56021a36a5f6423a8881f0b117a",
            ),
        ] {
            assert_eq!(hex::encode(grouped.key.to_bytes()), expected_key);
            assert_eq!(grouped.issue_id.to_string(), expected_issue);
        }
    }

    #[test]
    fn sdk_platform_golden_corpus_uses_semantic_components_not_platform_label() {
        let project = ProjectId::new(42).unwrap();
        let platforms = [
            EventPlatform::Python,
            EventPlatform::JavaScript,
            EventPlatform::Node,
            EventPlatform::Java,
            EventPlatform::DotNet,
            EventPlatform::Go,
            EventPlatform::Rust,
            EventPlatform::Php,
            EventPlatform::Ruby,
            EventPlatform::Cocoa,
            EventPlatform::Dart,
        ];
        for platform in platforms {
            let mut event = body();
            event.platform = platform;
            event.exceptions = vec![exception(
                "RuntimeError",
                vec![frame("main", "/app/src/main.py")],
            )];
            let grouped = group(project, 1, &event, None).unwrap();
            assert_eq!(
                hex::encode(grouped.key.to_bytes()),
                "0001cbc968a7ddaee06919d0db32d78d9eff119836a2097cc98a1e759b6bc3efe87d"
            );
            assert_eq!(grouped.strategy, GroupingStrategy::ExceptionStack);
        }
    }

    #[test]
    fn canonical_encoding_is_deterministic_and_not_ambiguous_concatenation() {
        let first = vec![
            GroupingComponent {
                kind: GroupingComponentKind::SdkFingerprint,
                value: "ab".into(),
            },
            GroupingComponent {
                kind: GroupingComponentKind::SdkFingerprint,
                value: "c".into(),
            },
        ];
        let second = vec![
            GroupingComponent {
                kind: GroupingComponentKind::SdkFingerprint,
                value: "a".into(),
            },
            GroupingComponent {
                kind: GroupingComponentKind::SdkFingerprint,
                value: "bc".into(),
            },
        ];
        assert_ne!(
            key_for(1, GroupingStrategy::SdkFingerprint, &first),
            key_for(1, GroupingStrategy::SdkFingerprint, &second)
        );
        for seed in 0..256_u64 {
            let mut event = body();
            event.message = Some(format!("failure code 500 request {seed:016x}").into());
            let first = group(ProjectId::new(42).unwrap(), 1, &event, None).unwrap();
            let second = group(ProjectId::new(42).unwrap(), 1, &event, None).unwrap();
            assert_eq!(first, second, "seed {seed}");
        }
    }

    #[test]
    fn javascript_derived_frames_change_exception_grouping_semantically() {
        let project = ProjectId::new(42).unwrap();
        let mut event = body();
        event.platform = EventPlatform::JavaScript;
        event.exceptions = vec![exception(
            "TypeError",
            vec![frame("minified", "assets/app.min.js")],
        )];
        let raw = vec![crate::symbolication::RawStacktrace {
            origin: RawTraceOrigin::Exception { index: 0 },
            frames: event.exceptions[0].stacktrace.clone(),
        }];
        let mapped = |function: &str| SymbolicationResult {
            kind: crate::symbolication::SymbolicationKind::JavaScript,
            status: SymbolicationStatus::Complete,
            disposition: SymbolicationDisposition::Continue,
            raw: raw.clone(),
            derived: vec![SymbolicatedStacktrace {
                origin: RawTraceOrigin::Exception { index: 0 },
                frames: vec![SymbolicatedFrame {
                    original_index: 0,
                    function: Some(function.into()),
                    filename: Some("src/app.ts".into()),
                    module: Some("frontend".into()),
                    line: Some(10),
                    column: Some(2),
                }],
            }],
            missing_debug_ids: Vec::new(),
            diagnostics: Vec::new(),
        };
        let first = group(project, 1, &event, Some(&mapped("checkout"))).unwrap();
        let second = group(project, 1, &event, Some(&mapped("login"))).unwrap();
        assert_eq!(first.strategy, GroupingStrategy::ExceptionStack);
        assert_ne!(first.key, second.key);
    }

    #[test]
    #[ignore = "Phase 7 Grouper CPU/component baseline runs in release mode"]
    fn performance_grouper_revision_1_rps() {
        let project = ProjectId::new(42).unwrap();
        let message = body();
        let mut exception_body = body();
        exception_body.exceptions = vec![exception(
            "RuntimeError",
            vec![frame("outer", "src/lib.py"), frame("inner", "src/main.py")],
        )];
        let native = native_body("DEBUG-A", "0x1010");
        let mut fingerprint = body();
        fingerprint.fingerprint = vec!["billing".into(), DEFAULT_FINGERPRINT.into()];
        let corpus = [message, exception_body, native, fingerprint];
        let iterations = 200_000_u64;
        let started = std::time::Instant::now();
        let mut component_bytes = 0_usize;
        for index in 0..iterations {
            let grouped = group(project, 1, &corpus[index as usize % corpus.len()], None).unwrap();
            if index < corpus.len() as u64 {
                component_bytes += grouped
                    .explanation
                    .components
                    .iter()
                    .map(|component| component.value.len() + 5)
                    .sum::<usize>();
            }
            std::hint::black_box(grouped);
        }
        let rps = iterations as f64 / started.elapsed().as_secs_f64();
        eprintln!(
            "Grouper Phase 7: rps={rps:.0},events={iterations},corpus_component_bytes={component_bytes}"
        );
        assert!(rps >= 20_000.0, "Grouper {rps:.0} RPS is below gate");
    }
}
