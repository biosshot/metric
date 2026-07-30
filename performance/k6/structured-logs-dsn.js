import http from "k6/http";
import { check } from "k6";
import { Counter, Trend } from "k6/metrics";

const requests = new Counter("faultkeep_log_requests");
const accepted = new Counter("faultkeep_log_200");
const rateLimited = new Counter("faultkeep_log_429");
const unavailable = new Counter("faultkeep_log_503");
const otherStatus = new Counter("faultkeep_log_other");
const tcpErrors = new Counter("faultkeep_log_tcp_errors");
const duration = new Trend("faultkeep_log_duration", true);

const dsn = __ENV.FAULTKEEP_DSN;
if (!dsn) {
  throw new Error("FAULTKEEP_DSN is required (for example: http://<key>@localhost:4001/795186066)");
}

const { endpoint, publicKey } = parseDsn(dsn);
const rate = Number(__ENV.FAULTKEEP_LOG_RPS || "100");
const testDuration = __ENV.FAULTKEEP_DURATION || "10s";
const runId = (__ENV.FAULTKEEP_RUN_ID || "00000001").padStart(8, "0").slice(-8);

export const options = {
  scenarios: {
    logs: {
      executor: "constant-arrival-rate",
      rate,
      timeUnit: "1s",
      duration: testDuration,
      preAllocatedVUs: Number(__ENV.FAULTKEEP_LOG_VUS || "64"),
      maxVUs: Number(__ENV.FAULTKEEP_MAX_VUS || "2048"),
    },
  },
  thresholds: {
    checks: ["rate==1"],
    http_req_failed: ["rate<0.001"],
    faultkeep_log_duration: ["p(95)<150", "p(99)<300"],
    dropped_iterations: ["count==0"],
  },
  discardResponseBodies: true,
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

function parseDsn(value) {
  const match = /^(https?):\/\/([^:@/?#]+)(?::[^@/?#]*)?@([^/?#]+)(\/[^?#]*)(?:[?#].*)?$/.exec(value);
  if (!match) {
    throw new Error("FAULTKEEP_DSN must be a valid URL");
  }
  const [, protocol, publicKey, host, path] = match;
  const segments = path.split("/").filter(Boolean);
  const projectId = segments.pop();
  if (!projectId || !/^\d+$/.test(projectId)) {
    throw new Error("FAULTKEEP_DSN path must end with a numeric Sentry project ID");
  }
  const prefix = segments.length ? `/${segments.join("/")}` : "";
  return {
    endpoint: `${protocol}://${host}${prefix}/api/${projectId}/envelope/`,
    publicKey: decodeURIComponent(publicKey),
  };
}

function identity() {
  const vu = __VU.toString(16).padStart(6, "0");
  const iteration = __ITER.toString(16).padStart(18, "0");
  return `${runId}${vu}${iteration}`.slice(-32);
}

export default function sendLog() {
  const id = identity();
  const body = JSON.stringify({
    version: 2,
    items: [
      {
        timestamp: Date.now() / 1000 + __ITER / 1_000_000,
        level: "info",
        body: `k6 structured log ${id}`,
        trace_id: id,
        attributes: {
          "sentry.trace.parent_span_id": { value: id.slice(0, 16), type: "string" },
          "sentry.environment": { value: "k6", type: "string" },
          "service.name": { value: "structured-log-load", type: "string" },
          sequence: { value: __ITER, type: "integer" },
        },
      },
    ],
  });
  const envelope = `{}\n{"type":"log","length":${body.length}}\n${body}`;
  requests.add(1);
  const response = http.post(endpoint, envelope, {
    headers: {
      "Content-Type": "application/x-sentry-envelope",
      "X-Sentry-Auth": `Sentry sentry_version=7,sentry_client=faultkeep-k6/1,sentry_key=${publicKey}`,
    },
    tags: { signal: "log" },
  });
  duration.add(response.timings.duration);
  if (response.status === 0) tcpErrors.add(1);
  else if (response.status === 200) accepted.add(1);
  else if (response.status === 429) rateLimited.add(1);
  else if (response.status === 503) unavailable.add(1);
  else otherStatus.add(1, { status: String(response.status) });
  check(response, { "Log durably accepted": (result) => result.status === 200 });
}
