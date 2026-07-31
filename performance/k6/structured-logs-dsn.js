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
const metrics = createSignalMetrics("log");
const rate = positiveNumber("FAULTKEEP_LOG_RPS", "100");

export const options = constantArrivalOptions({
  scenario: "logs",
  rate,
  preAllocatedVUs: positiveNumber("FAULTKEEP_LOG_VUS", "64"),
  durationTrend: "faultkeep_log_duration",
});

export default function sendLog() {
  const id = uniqueId();
  const body = JSON.stringify({
    version: 2,
    items: [
      {
        timestamp: Date.now() / 1000 + __ITER / 1_000_000,
        level: "info",
        body: `k6 structured log ${id}`,
        trace_id: id,
        attributes: {
          "sentry.trace.parent_span_id": {
            value: id.slice(0, 16),
            type: "string",
          },
          "sentry.environment": { value: "k6", type: "string" },
          "service.name": { value: "structured-log-load", type: "string" },
          sequence: { value: __ITER, type: "integer" },
        },
      },
    ],
  });
  const response = postEnvelope({
    target,
    itemType: "log",
    body,
    tags: { fixture: "faultkeep-structured-log-v1" },
  });
  recordResponse(metrics, response, "Log durably accepted");
}
