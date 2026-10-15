-- Raw Explore pages use descending time and identity for every dataset.
-- Existing error/log/span timelines already provide this ordering.
CREATE INDEX metric_explore_timeline ON metric.metric_buckets(project_id,bucket_start DESC,bucket_id DESC);
