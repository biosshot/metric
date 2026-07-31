import {
  constantArrivalOptions,
  createSignalMetrics,
  parseDsn,
  positiveNumber,
  postEnvelope,
  recordResponse,
  uniqueId,
} from "./dsn-load.js";

const target = parseDsn();
const metrics = createSignalMetrics("metric");
const rate = positiveNumber("FAULTKEEP_METRIC_RPS", "500");

export const options = constantArrivalOptions({
  scenario: "metrics",
  rate,
  preAllocatedVUs: positiveNumber("FAULTKEEP_METRIC_VUS", "64"),
  durationTrend: "faultkeep_metric_duration",
});

export default function sendMetrics() {
  const traceId = uniqueId();
  const timestamp = Date.now() / 1000;
  const attributes = {
    source: { value: "k6", type: "string" },
    scenario: { value: "metrics-dsn", type: "string" },
  };
  const body = JSON.stringify({
    version: 2,
    items: [
      {
        timestamp,
        trace_id: traceId,
        name: "faultkeep.k6.requests",
        type: "counter",
        unit: "none",
        value: 1,
        attributes,
      },
      {
        timestamp,
        trace_id: traceId,
        name: "faultkeep.k6.queue",
        type: "gauge",
        unit: "item",
        value: __VU,
        attributes,
      },
      {
        timestamp,
        trace_id: traceId,
        name: "faultkeep.k6.duration",
        type: "distribution",
        unit: "millisecond",
        value: (__ITER % 100) + 0.5,
        attributes,
      },
    ],
  });
  const response = postEnvelope({
    target,
    itemType: "trace_metric",
    body,
    itemHeaders: {
      content_type: "application/vnd.sentry.items.trace-metric+json",
      item_count: 3,
    },
    tags: { fixture: "faultkeep-trace-metric-v1" },
  });
  recordResponse(metrics, response, "Metric container durably accepted");
}
