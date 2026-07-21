use std::fmt;

/// Low-cardinality metric names registered by the Phase 0 facade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    HttpRequests,
    Shutdowns,
}

impl Metric {
    const fn name(self) -> &'static str {
        match self {
            Self::HttpRequests => "faultkeep_http_requests_total",
            Self::Shutdowns => "faultkeep_shutdown_total",
        }
    }
}

/// Bounded metric outcomes; arbitrary labels are intentionally impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
}

impl Outcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Exporter-independent metrics facade. Without an installed recorder it is a no-op.
#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics;

impl Metrics {
    pub fn increment(self, metric: Metric, outcome: Outcome) {
        metrics::counter!(metric.name(), "outcome" => outcome.label()).increment(1);
    }
}

/// Request correlation identifier generated at the transport boundary.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId([u8; 16]);

impl RequestId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_has_fixed_safe_shape() {
        let id = RequestId::from_bytes([0xab; 16]);
        assert_eq!(id.to_string(), "abababababababababababababababab");
    }

    #[test]
    fn metrics_facade_accepts_only_bounded_dimensions() {
        Metrics.increment(Metric::HttpRequests, Outcome::Ok);
    }
}
