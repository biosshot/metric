import http from "k6/http";
import { check } from "k6";

const target = __ENV.FAULTKEEP_TARGET || "http://127.0.0.1:3101";
const targetRps = Number(__ENV.FAULTKEEP_RPS || "1158");
const duration = __ENV.FAULTKEEP_DURATION || "30s";
const runId = (__ENV.FAULTKEEP_RUN_ID || "00000001").padStart(8, "0").slice(-8);
const resultPath = __ENV.FAULTKEEP_RESULT || "performance/results/ingest-mongodb.json";

export const options = {
  scenarios: {
    ingest: {
      executor: "constant-arrival-rate",
      rate: targetRps,
      timeUnit: "1s",
      duration,
      preAllocatedVUs: Number(__ENV.FAULTKEEP_PREALLOCATED_VUS || "128"),
      maxVUs: Number(__ENV.FAULTKEEP_MAX_VUS || "2048"),
    },
  },
  thresholds: {
    checks: ["rate==1"],
    http_req_failed: ["rate<0.001"],
    http_req_duration: ["p(95)<100", "p(99)<250"],
    dropped_iterations: ["count==0"],
  },
  discardResponseBodies: true,
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

function eventId() {
  const vu = __VU.toString(16).padStart(6, "0");
  const iteration = __ITER.toString(16).padStart(18, "0");
  return `${runId}${vu}${iteration}`.slice(-32);
}

export default function () {
  const id = eventId();
  const event = JSON.stringify({
    event_id: id,
    platform: "javascript",
    level: "error",
    message: "k6 durable synthetic failure",
    exception: {
      values: [{ type: "SyntheticError", value: "bounded fixture", stacktrace: { frames: [] } }],
    },
    request: {
      url: "https://user:password@example.invalid/path",
      headers: { authorization: "Bearer k6-secret", cookie: "session=k6-secret" },
    },
    user: { ip_address: "192.0.2.10" },
    extra: { padding: "x".repeat(512) },
  });
  const envelope = `{"event_id":"${id}"}\n{"type":"event","length":${event.length}}\n${event}`;
  const response = http.post(`${target}/api/42/envelope/`, envelope, {
    headers: {
      "Content-Type": "application/x-sentry-envelope",
      "X-Sentry-Auth":
        "Sentry sentry_version=7,sentry_client=faultkeep-k6/1,sentry_key=0123456789abcdef0123456789abcdef",
    },
    tags: { fixture: "error-event-v1-mongodb" },
  });
  check(response, { "durable MongoDB accepted": (result) => result.status === 200 });
}

export function handleSummary(data) {
  const durationMetric = data.metrics.http_req_duration?.values || {};
  const iterationMetric = data.metrics.iterations?.values || {};
  const artifact = {
    schema_version: 1,
    metadata: {
      commit: __ENV.FAULTKEEP_COMMIT || "working-tree",
      fixture_revision: "error-event-v1-mongodb",
      target_rps: targetRps,
      duration,
      run_id: runId,
      rust_toolchain: __ENV.FAULTKEEP_RUST || "unknown",
      k6_version: __ENV.FAULTKEEP_K6 || "unknown",
      hardware: __ENV.FAULTKEEP_HARDWARE || "unrecorded",
      mongo: __ENV.FAULTKEEP_MONGO || "MongoDB 8.0.12 standalone",
      durability: "MongoWriter unordered insert_many to MongoDB",
    },
    metrics: {
      iterations: iterationMetric.count || 0,
      achieved_rps: iterationMetric.rate || 0,
      error_rate: data.metrics.http_req_failed?.values?.rate || 0,
      latency_ms: {
        average: durationMetric.avg || 0,
        p50: durationMetric.med || 0,
        p95: durationMetric["p(95)"] || 0,
        p99: durationMetric["p(99)"] || 0,
        maximum: durationMetric.max || 0,
      },
      dropped_iterations: data.metrics.dropped_iterations?.values?.count || 0,
    },
  };
  return {
    [resultPath]: JSON.stringify(artifact, null, 2),
    stdout: `${JSON.stringify(artifact.metrics)}\nresult: ${resultPath}\n`,
  };
}
