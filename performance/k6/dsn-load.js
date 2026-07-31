import http from "k6/http";
import { check } from "k6";
import { Counter, Trend } from "k6/metrics";

export function parseDsn() {
  const value = __ENV.FAULTKEEP_DSN;
  if (!value) {
    throw new Error(
      "FAULTKEEP_DSN is required (for example: http://<key>@localhost:4001/795186066)",
    );
  }
  const match =
    /^(https?):\/\/([^:@/?#]+)(?::[^@/?#]*)?@([^/?#]+)(\/[^?#]*)(?:[?#].*)?$/.exec(
      value,
    );
  if (!match) {
    throw new Error("FAULTKEEP_DSN must be a valid URL");
  }
  const [, protocol, encodedPublicKey, host, path] = match;
  const segments = path.split("/").filter(Boolean);
  const projectId = segments.pop();
  if (!projectId || !/^\d+$/.test(projectId)) {
    throw new Error(
      "FAULTKEEP_DSN path must end with a numeric Sentry project ID",
    );
  }
  const prefix = segments.length ? `/${segments.join("/")}` : "";
  return {
    endpoint: `${protocol}://${host}${prefix}/api/${projectId}/envelope/`,
    publicKey: decodeURIComponent(encodedPublicKey),
  };
}

export function positiveNumber(name, fallback) {
  const value = Number(__ENV[name] || fallback);
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`${name} must be a positive number`);
  }
  return value;
}

export function constantArrivalOptions({
  scenario,
  rate,
  preAllocatedVUs,
  durationTrend,
  p95 = 150,
  p99 = 300,
}) {
  return {
    scenarios: {
      [scenario]: {
        executor: "constant-arrival-rate",
        rate,
        timeUnit: "1s",
        duration: __ENV.FAULTKEEP_DURATION || "10s",
        preAllocatedVUs,
        maxVUs: positiveNumber("FAULTKEEP_MAX_VUS", "2048"),
      },
    },
    thresholds: {
      checks: ["rate==1"],
      http_req_failed: ["rate<0.001"],
      [durationTrend]: [`p(95)<${p95}`, `p(99)<${p99}`],
      dropped_iterations: ["count==0"],
    },
    discardResponseBodies: true,
    summaryTrendStats: ["avg", "min", "med", "max", "p(90)", "p(95)", "p(99)"],
  };
}

export function createSignalMetrics(signal) {
  return {
    requests: new Counter(`faultkeep_${signal}_requests`),
    httpResponses: new Counter(`faultkeep_${signal}_http_responses`),
    accepted: new Counter(`faultkeep_${signal}_200`),
    rateLimited: new Counter(`faultkeep_${signal}_429`),
    unavailable: new Counter(`faultkeep_${signal}_503`),
    otherStatus: new Counter(`faultkeep_${signal}_other`),
    tcpErrors: new Counter(`faultkeep_${signal}_tcp_errors`),
    duration: new Trend(`faultkeep_${signal}_duration`, true),
  };
}

export function uniqueId() {
  const timestamp = Date.now().toString(16).padStart(12, "0").slice(-12);
  const vu = __VU.toString(16).padStart(4, "0").slice(-4);
  const iteration = __ITER.toString(16).padStart(16, "0").slice(-16);
  return `${timestamp}${vu}${iteration}`;
}

export function postEnvelope({
  target,
  itemType,
  body,
  eventId,
  itemHeaders = {},
  tags = {},
}) {
  const envelopeHeader = eventId ? { event_id: eventId } : {};
  const itemHeader = {
    type: itemType,
    length: body.length,
    ...itemHeaders,
  };
  const envelope = `${JSON.stringify(envelopeHeader)}\n${JSON.stringify(itemHeader)}\n${body}`;
  return http.post(target.endpoint, envelope, {
    headers: {
      "Content-Type": "application/x-sentry-envelope",
      "X-Sentry-Auth": `Sentry sentry_version=7,sentry_client=faultkeep-k6/1,sentry_key=${target.publicKey}`,
    },
    tags: { signal: itemType, ...tags },
  });
}

export function recordResponse(metrics, response, acceptedLabel) {
  metrics.requests.add(1);
  metrics.duration.add(response.timings.duration);
  const isTcpError = response.status === 0;
  const isAccepted = response.status === 200;
  const isRateLimited = response.status === 429;
  const isUnavailable = response.status === 503;
  const isOther =
    !isTcpError && !isAccepted && !isRateLimited && !isUnavailable;
  metrics.httpResponses.add(isTcpError ? 0 : 1);
  metrics.tcpErrors.add(isTcpError ? 1 : 0);
  metrics.accepted.add(isAccepted ? 1 : 0);
  metrics.rateLimited.add(isRateLimited ? 1 : 0);
  metrics.unavailable.add(isUnavailable ? 1 : 0);
  metrics.otherStatus.add(isOther ? 1 : 0, {
    status: isOther ? String(response.status) : "none",
  });
  check(response, { [acceptedLabel]: (result) => result.status === 200 });
}
