import http from "k6/http";
import { check } from "k6";
import { Counter, Trend } from "k6/metrics";

const tcpErrors = new Counter("faultkeep_tcp_errors");
const httpResponses = new Counter("faultkeep_http_responses");
const http200 = new Counter("faultkeep_http_200");
const http429 = new Counter("faultkeep_http_429");
const http503 = new Counter("faultkeep_http_503");
const httpOther = new Counter("faultkeep_http_other");
const logRequests = new Counter("faultkeep_log_requests");
const log200 = new Counter("faultkeep_log_200");
const errorRequests = new Counter("faultkeep_error_requests");
const error200 = new Counter("faultkeep_error_200");
const logDuration = new Trend("faultkeep_log_duration", true);
const errorDuration = new Trend("faultkeep_error_duration", true);

const target = __ENV.FAULTKEEP_TARGET || "http://127.0.0.1:3101";
const logRps = Number(__ENV.FAULTKEEP_LOG_RPS || "1000");
const errorRps = Number(__ENV.FAULTKEEP_ERROR_RPS || "250");
const duration = __ENV.FAULTKEEP_DURATION || "10s";
const runId = (__ENV.FAULTKEEP_RUN_ID || "00000001").padStart(8, "0").slice(-8);
const resultPath =
  __ENV.FAULTKEEP_RESULT || "performance/results/structured-logs.json";
const fixtureRevision = "phase24-node-log-container-v2-mixed-error-v1";

export const options = {
  scenarios: {
    logs: {
      executor: "constant-arrival-rate",
      exec: "sendLog",
      rate: logRps,
      timeUnit: "1s",
      duration,
      preAllocatedVUs: Number(__ENV.FAULTKEEP_LOG_VUS || "128"),
      maxVUs: Number(__ENV.FAULTKEEP_MAX_VUS || "2048"),
    },
    errors: {
      executor: "constant-arrival-rate",
      exec: "sendError",
      rate: errorRps,
      timeUnit: "1s",
      duration,
      preAllocatedVUs: Number(__ENV.FAULTKEEP_ERROR_VUS || "64"),
      maxVUs: Number(__ENV.FAULTKEEP_MAX_VUS || "2048"),
    },
  },
  thresholds: {
    checks: ["rate==1"],
    http_req_failed: ["rate<0.001"],
    faultkeep_log_duration: ["p(95)<150", "p(99)<300"],
    faultkeep_error_duration: ["p(95)<150", "p(99)<300"],
    dropped_iterations: ["count==0"],
  },
  discardResponseBodies: true,
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

function identity() {
  const vu = __VU.toString(16).padStart(6, "0");
  const iteration = __ITER.toString(16).padStart(18, "0");
  return `${runId}${vu}${iteration}`.slice(-32);
}

function envelopeRequest(itemType, body, eventId) {
  const envelopeHeader = eventId ? `{"event_id":"${eventId}"}` : "{}";
  const envelope = `${envelopeHeader}\n{"type":"${itemType}","length":${body.length}}\n${body}`;
  return http.post(`${target}/api/42/envelope/`, envelope, {
    headers: {
      "Content-Type": "application/x-sentry-envelope",
      "X-Sentry-Auth":
        "Sentry sentry_version=7,sentry_client=faultkeep-k6/1,sentry_key=0123456789abcdef0123456789abcdef",
    },
    tags: { fixture: fixtureRevision, signal: itemType },
  });
}

function accountResponse(response) {
  if (response.status === 0) {
    tcpErrors.add(1);
    return;
  }
  httpResponses.add(1);
  if (response.status === 200) http200.add(1);
  else if (response.status === 429) http429.add(1);
  else if (response.status === 503) http503.add(1);
  else httpOther.add(1, { status: String(response.status) });
}

export function sendLog() {
  const id = identity();
  const timestamp = Date.now() / 1000 + __ITER / 1_000_000;
  const body = JSON.stringify({
    version: 2,
    items: [
      {
        timestamp,
        level: "info",
        body: `phase24 structured log ${id}`,
        trace_id: id,
        attributes: {
          "sentry.trace.parent_span_id": {
            value: id.slice(0, 16),
            type: "string",
          },
          "sentry.environment": { value: "performance", type: "string" },
          "sentry.release": { value: "faultkeep@phase24", type: "string" },
          "service.name": { value: "log-load", type: "string" },
          sequence: { value: __ITER, type: "integer" },
        },
      },
    ],
  });
  logRequests.add(1);
  const response = envelopeRequest("log", body);
  logDuration.add(response.timings.duration);
  accountResponse(response);
  if (response.status === 200) log200.add(1);
  check(response, { "Log durably accepted": (result) => result.status === 200 });
}

export function sendError() {
  const id = identity();
  const body = JSON.stringify({
    event_id: id,
    platform: "javascript",
    level: "error",
    message: `phase24 isolation error ${id}`,
    exception: {
      values: [
        {
          type: "Phase24IsolationError",
          value: "Error lane remains responsive during Log ingest",
          stacktrace: { frames: [] },
        },
      ],
    },
  });
  errorRequests.add(1);
  const response = envelopeRequest("event", body, id);
  errorDuration.add(response.timings.duration);
  accountResponse(response);
  if (response.status === 200) error200.add(1);
  check(response, { "Error durably accepted": (result) => result.status === 200 });
}

function metricValues(data, name) {
  return data.metrics[name]?.values || {};
}

export function handleSummary(data) {
  const logLatency = metricValues(data, "faultkeep_log_duration");
  const errorLatency = metricValues(data, "faultkeep_error_duration");
  const artifact = {
    schema_version: 1,
    metadata: {
      scenario: "phase-24-structured-logs-mixed-http",
      commit: __ENV.FAULTKEEP_COMMIT || "working-tree",
      fixture_revision: fixtureRevision,
      log_target_rps: logRps,
      error_target_rps: errorRps,
      duration,
      run_id: runId,
      rust_toolchain: __ENV.FAULTKEEP_RUST || "unknown",
      k6_version: __ENV.FAULTKEEP_K6 || "unknown",
      hardware: __ENV.FAULTKEEP_HARDWARE || "unrecorded",
      mongo: __ENV.FAULTKEEP_MONGO || "MongoDB local standalone",
      durability:
        "dedicated LogWriter and Event MongoWriter using unordered insert_many",
    },
    metrics: {
      log_requests: metricValues(data, "faultkeep_log_requests").count || 0,
      log_achieved_rps: metricValues(data, "faultkeep_log_requests").rate || 0,
      log_status_200: metricValues(data, "faultkeep_log_200").count || 0,
      error_requests: metricValues(data, "faultkeep_error_requests").count || 0,
      error_achieved_rps: metricValues(data, "faultkeep_error_requests").rate || 0,
      error_status_200: metricValues(data, "faultkeep_error_200").count || 0,
      log_latency_ms: {
        average: logLatency.avg || 0,
        p95: logLatency["p(95)"] || 0,
        p99: logLatency["p(99)"] || 0,
        maximum: logLatency.max || 0,
      },
      error_latency_ms: {
        average: errorLatency.avg || 0,
        p95: errorLatency["p(95)"] || 0,
        p99: errorLatency["p(99)"] || 0,
        maximum: errorLatency.max || 0,
      },
      dropped_iterations:
        metricValues(data, "dropped_iterations").count || 0,
      failures: {
        tcp_errors: metricValues(data, "faultkeep_tcp_errors").count || 0,
        http_responses: metricValues(data, "faultkeep_http_responses").count || 0,
        status_200: metricValues(data, "faultkeep_http_200").count || 0,
        status_429: metricValues(data, "faultkeep_http_429").count || 0,
        status_503: metricValues(data, "faultkeep_http_503").count || 0,
        status_other: metricValues(data, "faultkeep_http_other").count || 0,
      },
    },
  };
  return {
    [resultPath]: `${JSON.stringify(artifact, null, 2)}\n`,
    stdout: `${JSON.stringify(artifact.metrics)}\nresult: ${resultPath}\n`,
  };
}
