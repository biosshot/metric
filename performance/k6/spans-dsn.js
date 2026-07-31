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
const metrics = createSignalMetrics("span");
const rate = positiveNumber("FAULTKEEP_SPAN_RPS", "500");

export const options = constantArrivalOptions({
  scenario: "spans",
  rate,
  preAllocatedVUs: positiveNumber("FAULTKEEP_SPAN_VUS", "64"),
  durationTrend: "faultkeep_span_duration",
});

export default function sendSpan() {
  const traceId = uniqueId();
  const spanId = traceId.slice(-16);
  const endedAt = Date.now() / 1000;
  const body = JSON.stringify({
    items: [
      {
        trace_id: traceId,
        span_id: spanId,
        start_timestamp: endedAt - 0.025,
        end_timestamp: endedAt,
        is_segment: true,
        op: "http.server",
        name: "GET /k6/spans",
        status: "ok",
        attributes: {
          "service.name": { value: "faultkeep-k6", type: "string" },
          "sentry.environment": { value: "k6", type: "string" },
          "sentry.release": {
            value: "faultkeep-k6-spans@1.0.0",
            type: "string",
          },
          "http.request.method": { value: "GET", type: "string" },
        },
      },
    ],
  });
  const response = postEnvelope({
    target,
    itemType: "span",
    body,
    itemHeaders: { item_count: 1 },
    tags: { fixture: "faultkeep-span-v1" },
  });
  recordResponse(metrics, response, "Span durably accepted");
}
