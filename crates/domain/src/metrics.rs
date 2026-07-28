//! Compact application-metric deltas crossing the dedicated durable lane.

use std::collections::{BTreeMap, btree_map::Entry};

use crate::{ProjectId, Timestamp, signals::TraceId};

pub const METRIC_SKETCH_BINS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetricKind {
    Counter,
    Gauge,
    Distribution,
}

impl MetricKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Distribution => "distribution",
        }
    }

    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Counter => 1,
            Self::Gauge => 2,
            Self::Distribution => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MetricSeries {
    pub project_id: ProjectId,
    pub name: Box<str>,
    pub kind: MetricKind,
    pub unit: Box<str>,
    pub tags: BTreeMap<Box<str>, Box<str>>,
}

impl MetricSeries {
    #[must_use]
    pub fn id(&self) -> [u8; 16] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"application-metric-series/v1");
        hasher.update(&self.project_id.get().to_be_bytes());
        hasher.update(self.name.as_bytes());
        hasher.update(&[0]);
        hasher.update(self.kind.as_str().as_bytes());
        hasher.update(&[0]);
        hasher.update(self.unit.as_bytes());
        for (key, value) in &self.tags {
            hasher.update(&[0]);
            hasher.update(key.as_bytes());
            hasher.update(&[0]);
            hasher.update(value.as_bytes());
        }
        let mut id = [0_u8; 16];
        id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetricAggregate {
    Counter {
        sum: f64,
        count: u64,
    },
    Gauge {
        last: f64,
        min: f64,
        max: f64,
        sum: f64,
        count: u64,
    },
    Distribution {
        min: f64,
        max: f64,
        sum: f64,
        count: u64,
        bins: Box<[u32; METRIC_SKETCH_BINS]>,
    },
}

impl MetricAggregate {
    #[must_use]
    pub fn from_measurement(kind: MetricKind, value: f64) -> Self {
        match kind {
            MetricKind::Counter => Self::Counter {
                sum: value,
                count: 1,
            },
            MetricKind::Gauge => Self::Gauge {
                last: value,
                min: value,
                max: value,
                sum: value,
                count: 1,
            },
            MetricKind::Distribution => {
                let mut bins = [0; METRIC_SKETCH_BINS];
                bins[sketch_bin(value)] = 1;
                Self::Distribution {
                    min: value,
                    max: value,
                    sum: value,
                    count: 1,
                    bins: Box::new(bins),
                }
            }
        }
    }

    pub fn merge(&mut self, newer: Self) -> bool {
        match (self, newer) {
            (
                Self::Counter { sum, count },
                Self::Counter {
                    sum: other_sum,
                    count: other_count,
                },
            ) => {
                *sum += other_sum;
                *count = count.saturating_add(other_count);
                true
            }
            (
                Self::Gauge {
                    last,
                    min,
                    max,
                    sum,
                    count,
                },
                Self::Gauge {
                    last: other_last,
                    min: other_min,
                    max: other_max,
                    sum: other_sum,
                    count: other_count,
                },
            ) => {
                *last = other_last;
                *min = min.min(other_min);
                *max = max.max(other_max);
                *sum += other_sum;
                *count = count.saturating_add(other_count);
                true
            }
            (
                Self::Distribution {
                    min,
                    max,
                    sum,
                    count,
                    bins,
                },
                Self::Distribution {
                    min: other_min,
                    max: other_max,
                    sum: other_sum,
                    count: other_count,
                    bins: other_bins,
                },
            ) => {
                *min = min.min(other_min);
                *max = max.max(other_max);
                *sum += other_sum;
                *count = count.saturating_add(other_count);
                for (bin, other) in bins.iter_mut().zip(other_bins.iter()) {
                    *bin = bin.saturating_add(*other);
                }
                true
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricDelta {
    pub series: MetricSeries,
    pub bucket_start: Timestamp,
    pub bucket_width_seconds: u32,
    pub received_at: Timestamp,
    pub trace_id: Option<TraceId>,
    pub aggregate: MetricAggregate,
}

impl MetricDelta {
    #[must_use]
    pub fn bucket_id(&self) -> [u8; 16] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"application-metric-bucket/v1");
        hasher.update(&self.series.id());
        hasher.update(&self.bucket_start.unix_millis().to_be_bytes());
        hasher.update(&self.bucket_width_seconds.to_be_bytes());
        let mut id = [0_u8; 16];
        id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        id
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetricDeltaBatch {
    pub deltas: BTreeMap<([u8; 16], i64), MetricDelta>,
    pub source_measurements: u32,
    pub discarded_measurements: u32,
}

impl MetricDeltaBatch {
    #[must_use]
    pub fn len(&self) -> usize {
        self.deltas.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    pub fn push(&mut self, delta: MetricDelta) {
        let key = (delta.series.id(), delta.bucket_start.unix_millis());
        match self.deltas.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(delta);
            }
            Entry::Occupied(mut entry) => {
                let current = entry.get_mut();
                if current.aggregate.merge(delta.aggregate) {
                    current.received_at = delta.received_at;
                    if delta.trace_id.is_some() {
                        current.trace_id = delta.trace_id;
                    }
                }
            }
        }
    }

    pub fn merge(&mut self, newer: Self) {
        self.discarded_measurements = self
            .discarded_measurements
            .saturating_add(newer.discarded_measurements);
        self.source_measurements = self
            .source_measurements
            .saturating_add(newer.source_measurements);
        for delta in newer.deltas.into_values() {
            self.push(delta);
        }
    }
}

#[must_use]
pub fn sketch_bin(value: f64) -> usize {
    if value == 0.0 {
        return 31;
    }
    let magnitude = value.abs().log2().floor().clamp(-30.0, 31.0) as i32;
    usize::try_from(magnitude + 31).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_sketch_is_bounded_and_mergeable() {
        let mut left = MetricAggregate::from_measurement(MetricKind::Distribution, 1.0);
        assert!(left.merge(MetricAggregate::from_measurement(
            MetricKind::Distribution,
            8.0
        )));
        let MetricAggregate::Distribution {
            min,
            max,
            count,
            bins,
            ..
        } = left
        else {
            panic!("distribution");
        };
        assert_eq!((min, max, count), (1.0, 8.0, 2));
        assert_eq!(bins.iter().copied().sum::<u32>(), 2);
    }
}
