//! Bounded, allocation-free-on-match project inbound filters.

use std::fmt;

pub const MAX_INBOUND_FILTER_RULES: usize = 32;
pub const MAX_INBOUND_FILTER_PATTERN_BYTES: usize = 256;
pub const MAX_COMPILED_INBOUND_FILTER_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundFilterSignal {
    Error,
    Log,
    Transaction,
    Span,
}

impl InboundFilterSignal {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Log => "log",
            Self::Transaction => "transaction",
            Self::Span => "span",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundFilterField {
    Release,
    Environment,
    Service,
    Message,
    ExceptionType,
    Logger,
    RequestHost,
    RequestPath,
    Severity,
    Name,
    Operation,
    Status,
    Duration,
}

impl InboundFilterField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Environment => "environment",
            Self::Service => "service",
            Self::Message => "message",
            Self::ExceptionType => "exception_type",
            Self::Logger => "logger",
            Self::RequestHost => "request_host",
            Self::RequestPath => "request_path",
            Self::Severity => "severity",
            Self::Name => "name",
            Self::Operation => "operation",
            Self::Status => "status",
            Self::Duration => "duration",
        }
    }

    #[must_use]
    pub const fn accepted_by(self, signal: InboundFilterSignal) -> bool {
        match self {
            Self::Release | Self::Environment | Self::Service => true,
            Self::Message => matches!(
                signal,
                InboundFilterSignal::Error | InboundFilterSignal::Log
            ),
            Self::ExceptionType | Self::Logger | Self::RequestHost | Self::RequestPath => {
                matches!(signal, InboundFilterSignal::Error)
            }
            Self::Severity => matches!(signal, InboundFilterSignal::Log),
            Self::Name | Self::Operation | Self::Status | Self::Duration => {
                matches!(
                    signal,
                    InboundFilterSignal::Transaction | InboundFilterSignal::Span
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundFilterOperation {
    Exact,
    Prefix,
    Suffix,
    Contains,
    Glob,
}

impl InboundFilterOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Suffix => "suffix",
            Self::Contains => "contains",
            Self::Glob => "glob",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundFilterRule {
    pub signal: InboundFilterSignal,
    pub field: InboundFilterField,
    pub operation: InboundFilterOperation,
    pub pattern: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboundFilterPolicy {
    rules: Vec<InboundFilterRule>,
}

impl InboundFilterPolicy {
    pub fn new(rules: Vec<InboundFilterRule>) -> Result<Self, InboundFilterError> {
        let policy = Self { rules };
        let _ = policy.compile()?;
        Ok(policy)
    }

    #[must_use]
    pub fn rules(&self) -> &[InboundFilterRule] {
        &self.rules
    }

    pub fn compile(&self) -> Result<CompiledInboundFilterPolicy, InboundFilterError> {
        if self.rules.len() > MAX_INBOUND_FILTER_RULES {
            return Err(InboundFilterError::TooManyRules);
        }
        let mut compiled_bytes = 0_usize;
        let mut compiled = CompiledInboundFilterPolicy::default();
        for rule in &self.rules {
            if !rule.field.accepted_by(rule.signal) {
                return Err(InboundFilterError::FieldUnavailable);
            }
            if rule.pattern.is_empty()
                || rule.pattern.len() > MAX_INBOUND_FILTER_PATTERN_BYTES
                || rule.pattern.chars().any(char::is_control)
            {
                return Err(InboundFilterError::InvalidPattern);
            }
            if rule.field == InboundFilterField::Duration
                && rule.operation != InboundFilterOperation::Exact
            {
                return Err(InboundFilterError::InvalidDurationOperation);
            }
            let matcher = CompiledMatcher::compile(rule.field, rule.operation, &rule.pattern)?;
            compiled_bytes = compiled_bytes
                .checked_add(matcher.compiled_bytes())
                .ok_or(InboundFilterError::PolicyTooLarge)?;
            if compiled_bytes > MAX_COMPILED_INBOUND_FILTER_BYTES {
                return Err(InboundFilterError::PolicyTooLarge);
            }
            compiled
                .rules_mut(rule.signal)
                .push(CompiledInboundFilterRule {
                    signal: rule.signal,
                    field: rule.field,
                    matcher,
                });
        }
        Ok(compiled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompiledInboundFilterPolicy {
    error: Vec<CompiledInboundFilterRule>,
    log: Vec<CompiledInboundFilterRule>,
    transaction: Vec<CompiledInboundFilterRule>,
    span: Vec<CompiledInboundFilterRule>,
}

impl CompiledInboundFilterPolicy {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.error.is_empty()
            && self.log.is_empty()
            && self.transaction.is_empty()
            && self.span.is_empty()
    }

    #[must_use]
    pub fn has_signal(&self, signal: InboundFilterSignal) -> bool {
        !self.rules(signal).is_empty()
    }

    #[must_use]
    pub fn has_field(&self, signal: InboundFilterSignal, field: InboundFilterField) -> bool {
        self.rules(signal).iter().any(|rule| rule.field == field)
    }

    #[must_use]
    pub fn matches(&self, fields: &InboundFilterFields<'_>) -> Option<InboundFilterMatch> {
        self.rules(fields.signal)
            .iter()
            .find(|rule| rule.matches(fields))
            .map(|rule| InboundFilterMatch {
                signal: rule.signal,
                field: rule.field,
            })
    }

    fn rules(&self, signal: InboundFilterSignal) -> &[CompiledInboundFilterRule] {
        match signal {
            InboundFilterSignal::Error => &self.error,
            InboundFilterSignal::Log => &self.log,
            InboundFilterSignal::Transaction => &self.transaction,
            InboundFilterSignal::Span => &self.span,
        }
    }

    fn rules_mut(&mut self, signal: InboundFilterSignal) -> &mut Vec<CompiledInboundFilterRule> {
        match signal {
            InboundFilterSignal::Error => &mut self.error,
            InboundFilterSignal::Log => &mut self.log,
            InboundFilterSignal::Transaction => &mut self.transaction,
            InboundFilterSignal::Span => &mut self.span,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InboundFilterFields<'a> {
    pub signal: InboundFilterSignal,
    pub release: Option<&'a str>,
    pub environment: Option<&'a str>,
    pub service: Option<&'a str>,
    pub message: Option<&'a str>,
    pub exception_type: Option<&'a str>,
    pub logger: Option<&'a str>,
    pub request_host: Option<&'a str>,
    pub request_path: Option<&'a str>,
    pub severity: Option<&'a str>,
    pub name: Option<&'a str>,
    pub operation: Option<&'a str>,
    pub status: Option<&'a str>,
    pub duration_ms: Option<i64>,
}

impl<'a> InboundFilterFields<'a> {
    #[must_use]
    pub const fn empty(signal: InboundFilterSignal) -> Self {
        Self {
            signal,
            release: None,
            environment: None,
            service: None,
            message: None,
            exception_type: None,
            logger: None,
            request_host: None,
            request_path: None,
            severity: None,
            name: None,
            operation: None,
            status: None,
            duration_ms: None,
        }
    }

    fn text(self, field: InboundFilterField) -> Option<&'a str> {
        match field {
            InboundFilterField::Release => self.release,
            InboundFilterField::Environment => self.environment,
            InboundFilterField::Service => self.service,
            InboundFilterField::Message => self.message,
            InboundFilterField::ExceptionType => self.exception_type,
            InboundFilterField::Logger => self.logger,
            InboundFilterField::RequestHost => self.request_host,
            InboundFilterField::RequestPath => self.request_path,
            InboundFilterField::Severity => self.severity,
            InboundFilterField::Name => self.name,
            InboundFilterField::Operation => self.operation,
            InboundFilterField::Status => self.status,
            InboundFilterField::Duration => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboundFilterMatch {
    pub signal: InboundFilterSignal,
    pub field: InboundFilterField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundFilterError {
    TooManyRules,
    FieldUnavailable,
    InvalidPattern,
    InvalidDuration,
    InvalidDurationOperation,
    PolicyTooLarge,
}

impl fmt::Display for InboundFilterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyRules => "inbound filter rule count exceeds the bound",
            Self::FieldUnavailable => "inbound filter field is unavailable for the signal",
            Self::InvalidPattern => "inbound filter pattern is invalid",
            Self::InvalidDuration => "duration filter must be an integer number of milliseconds",
            Self::InvalidDurationOperation => "duration filter supports exact matching only",
            Self::PolicyTooLarge => "compiled inbound filter policy exceeds the byte bound",
        })
    }
}

impl std::error::Error for InboundFilterError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledInboundFilterRule {
    signal: InboundFilterSignal,
    field: InboundFilterField,
    matcher: CompiledMatcher,
}

impl CompiledInboundFilterRule {
    fn matches(&self, fields: &InboundFilterFields<'_>) -> bool {
        if self.field == InboundFilterField::Duration {
            return fields
                .duration_ms
                .is_some_and(|value| self.matcher.matches_duration(value));
        }
        fields
            .text(self.field)
            .is_some_and(|value| self.matcher.matches(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompiledMatcher {
    Exact(Box<str>),
    Prefix(Box<str>),
    Suffix(Box<str>),
    Contains {
        pattern: Box<[u8]>,
        prefix: Box<[usize]>,
    },
    Glob(Box<str>),
    Duration(i64),
}

impl CompiledMatcher {
    fn compile(
        field: InboundFilterField,
        operation: InboundFilterOperation,
        pattern: &str,
    ) -> Result<Self, InboundFilterError> {
        if field == InboundFilterField::Duration {
            return pattern
                .parse::<i64>()
                .map(Self::Duration)
                .map_err(|_| InboundFilterError::InvalidDuration);
        }
        match operation {
            InboundFilterOperation::Exact => Ok(Self::Exact(pattern.into())),
            InboundFilterOperation::Prefix => Ok(Self::Prefix(pattern.into())),
            InboundFilterOperation::Suffix => Ok(Self::Suffix(pattern.into())),
            InboundFilterOperation::Contains => {
                let pattern = pattern.as_bytes().to_vec();
                let prefix = kmp_prefix(&pattern);
                Ok(Self::Contains {
                    pattern: pattern.into_boxed_slice(),
                    prefix: prefix.into_boxed_slice(),
                })
            }
            InboundFilterOperation::Glob => Ok(Self::Glob(pattern.into())),
        }
    }

    fn compiled_bytes(&self) -> usize {
        match self {
            Self::Exact(pattern)
            | Self::Prefix(pattern)
            | Self::Suffix(pattern)
            | Self::Glob(pattern) => pattern.len(),
            Self::Contains { pattern, prefix } => {
                pattern.len() + prefix.len() * std::mem::size_of::<usize>()
            }
            Self::Duration(_) => std::mem::size_of::<i64>(),
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Exact(pattern) => value == pattern.as_ref(),
            Self::Prefix(pattern) => value.starts_with(pattern.as_ref()),
            Self::Suffix(pattern) => value.ends_with(pattern.as_ref()),
            Self::Contains { pattern, prefix } => kmp_contains(value.as_bytes(), pattern, prefix),
            Self::Glob(pattern) => glob_matches(pattern, value),
            Self::Duration(_) => false,
        }
    }

    fn matches_duration(&self, value: i64) -> bool {
        matches!(self, Self::Duration(expected) if *expected == value)
    }
}

fn kmp_prefix(pattern: &[u8]) -> Vec<usize> {
    let mut prefix = vec![0; pattern.len()];
    let mut matched = 0_usize;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    prefix
}

fn kmp_contains(value: &[u8], pattern: &[u8], prefix: &[usize]) -> bool {
    let mut matched = 0_usize;
    for byte in value {
        while matched > 0 && *byte != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if *byte == pattern[matched] {
            matched += 1;
            if matched == pattern.len() {
                return true;
            }
        }
    }
    false
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut pattern_index, mut value_index) = (0_usize, 0_usize);
    let (mut star, mut star_value) = (None, 0_usize);
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            star_value = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_value += 1;
            value_index = star_value;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;
    use std::time::Instant;

    fn rule(
        signal: InboundFilterSignal,
        field: InboundFilterField,
        operation: InboundFilterOperation,
        pattern: &str,
    ) -> InboundFilterRule {
        InboundFilterRule {
            signal,
            field,
            operation,
            pattern: pattern.into(),
        }
    }

    #[test]
    fn typed_fields_and_bounds_are_enforced() {
        assert_eq!(
            InboundFilterPolicy::new(vec![rule(
                InboundFilterSignal::Log,
                InboundFilterField::ExceptionType,
                InboundFilterOperation::Exact,
                "SecretError",
            )]),
            Err(InboundFilterError::FieldUnavailable)
        );
        assert_eq!(
            InboundFilterPolicy::new(vec![rule(
                InboundFilterSignal::Span,
                InboundFilterField::Duration,
                InboundFilterOperation::Contains,
                "10",
            )]),
            Err(InboundFilterError::InvalidDurationOperation)
        );
    }

    #[test]
    fn all_text_matchers_are_deterministic() {
        let cases = [
            (InboundFilterOperation::Exact, "worker.failed", true),
            (InboundFilterOperation::Prefix, "worker.", true),
            (InboundFilterOperation::Suffix, ".failed", true),
            (InboundFilterOperation::Contains, "ker.fa", true),
            (InboundFilterOperation::Glob, "wor*.fa?led", true),
            (InboundFilterOperation::Glob, "api-*", false),
        ];
        for (operation, pattern, expected) in cases {
            let compiled = InboundFilterPolicy::new(vec![rule(
                InboundFilterSignal::Log,
                InboundFilterField::Message,
                operation,
                pattern,
            )])
            .expect("valid policy")
            .compile()
            .expect("compiled policy");
            let mut fields = InboundFilterFields::empty(InboundFilterSignal::Log);
            fields.message = Some("worker.failed");
            assert_eq!(compiled.matches(&fields).is_some(), expected);
        }
    }

    #[test]
    fn compiled_rules_are_partitioned_by_signal() {
        let compiled = InboundFilterPolicy::new(vec![rule(
            InboundFilterSignal::Log,
            InboundFilterField::Message,
            InboundFilterOperation::Exact,
            "same text",
        )])
        .expect("valid policy")
        .compile()
        .expect("compiled policy");
        assert!(compiled.has_signal(InboundFilterSignal::Log));
        assert!(!compiled.has_signal(InboundFilterSignal::Error));
        let mut error = InboundFilterFields::empty(InboundFilterSignal::Error);
        error.message = Some("same text");
        assert!(compiled.matches(&error).is_none());
    }

    #[test]
    fn glob_matches_exhaustive_small_alphabet_reference() {
        fn reference(pattern: &[u8], value: &[u8]) -> bool {
            match pattern.split_first() {
                None => value.is_empty(),
                Some((&b'*', rest)) => {
                    reference(rest, value)
                        || value
                            .split_first()
                            .is_some_and(|(_, tail)| reference(pattern, tail))
                }
                Some((&head, rest)) => value.split_first().is_some_and(|(&byte, tail)| {
                    (head == b'?' || head == byte) && reference(rest, tail)
                }),
            }
        }
        let patterns = ["", "*", "?", "a", "a*", "*b", "a?b", "*a*b"];
        let values = ["", "a", "b", "ab", "aab", "abb", "baab"];
        for pattern in patterns {
            for value in values {
                assert_eq!(
                    glob_matches(pattern, value),
                    reference(pattern.as_bytes(), value.as_bytes()),
                    "{pattern:?} {value:?}"
                );
            }
        }
    }

    #[test]
    #[ignore = "explicit RPS baseline; run with --ignored --nocapture"]
    fn performance_worst_case_policy_rps() {
        let rules = (0..MAX_INBOUND_FILTER_RULES)
            .map(|index| {
                rule(
                    InboundFilterSignal::Error,
                    InboundFilterField::Message,
                    InboundFilterOperation::Contains,
                    &format!("never-{index:02}-xxxxxxxx"),
                )
            })
            .collect();
        let compiled = InboundFilterPolicy::new(rules)
            .expect("bounded policy")
            .compile()
            .expect("compiled policy");
        let message = "a".repeat(8_192);
        let mut fields = InboundFilterFields::empty(InboundFilterSignal::Error);
        fields.message = Some(&message);
        let iterations = 10_000_u64;
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(compiled.matches(black_box(&fields)));
        }
        let elapsed = started.elapsed();
        let rps = iterations as f64 / elapsed.as_secs_f64();
        eprintln!(
            "inbound_filter_worst_case_rps={rps:.0} iterations={iterations} elapsed_ms={}",
            elapsed.as_millis()
        );
        assert!(
            rps >= 1_000.0,
            "worst-case matcher RPS regression: {rps:.0}"
        );
    }
}
