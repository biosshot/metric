import http from "k6/http";
import { check } from "k6";

const target = __ENV.METRIC_TARGET || "http://127.0.0.1:3100";
const targetRps = Number(__ENV.METRIC_RPS || "1000");
const duration = __ENV.METRIC_DURATION || "30s";
const mode = __ENV.METRIC_MODE || "arrival-rate";
const constantVUs = Number(__ENV.METRIC_VUS || "64");
const preAllocatedVUs = Number(__ENV.METRIC_PREALLOCATED_VUS || "64");
const maxVUs = Number(__ENV.METRIC_MAX_VUS || "512");
const resultPath = __ENV.METRIC_RESULT || "performance/results/ingest-fake.json";
const fixtureRevision = "error-event-v1";

export const options = {
  scenarios:
    mode === "max-throughput"
      ? {
          ingest: {
            executor: "constant-vus",
            vus: constantVUs,
            duration,
          },
        }
      : {
          ingest: {
            executor: "constant-arrival-rate",
            rate: targetRps,
            timeUnit: "1s",
            duration,
            preAllocatedVUs,
            maxVUs,
          },
        },
  thresholds:
    mode === "max-throughput"
      ? {
          checks: ["rate==1"],
          http_req_failed: ["rate<0.001"],
          http_req_duration: ["p(95)<100", "p(99)<250"],
        }
      : {
          checks: ["rate==1"],
          http_req_failed: ["rate<0.001"],
          http_req_duration: ["p(95)<100", "p(99)<250"],
          dropped_iterations: ["count==0"],
        },
  discardResponseBodies: true,
  summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
};

function eventId() {
  const vu = __VU.toString(16).padStart(8, "0");
  const iteration = __ITER.toString(16).padStart(24, "0");
  return `${vu}${iteration}`.slice(-32);
}

export default function () {
  const id = eventId();
  const event = JSON.stringify({
    event_id: id,
    message: "k6 synthetic failure",
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
        "Sentry sentry_version=7,sentry_client=metric-k6/1,sentry_key=0123456789abcdef0123456789abcdef",
    },
    tags: { fixture: fixtureRevision },
  });
  check(response, { "durable fake accepted": (result) => result.status === 200 });
}

export function handleSummary(data) {
  const durationMetric = data.metrics.http_req_duration?.values || {};
  const iterationMetric = data.metrics.iterations?.values || {};
  const failedMetric = data.metrics.http_req_failed?.values || {};
  const sentMetric = data.metrics.data_sent?.values || {};
  const receivedMetric = data.metrics.data_received?.values || {};
  const artifact = {
    schema_version: 1,
    metadata: {
      commit: __ENV.METRIC_COMMIT || "working-tree",
      fixture_revision: fixtureRevision,
      target_rps: targetRps,
      mode,
      constant_vus: mode === "max-throughput" ? constantVUs : null,
      duration,
      rust_toolchain: __ENV.METRIC_RUST || "unknown",
      k6_version: __ENV.METRIC_K6 || "unknown",
      hardware: __ENV.METRIC_HARDWARE || "unrecorded",
      command: "k6 run performance/k6/ingest-fake.js",
      durability: "deterministic benchmark fake; not MongoDB",
    },
    metrics: {
      iterations: iterationMetric.count || 0,
      achieved_rps: iterationMetric.rate || 0,
      error_rate: failedMetric.rate || 0,
      bytes_per_second: {
        sent: sentMetric.rate || 0,
        received: receivedMetric.rate || 0,
      },
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
