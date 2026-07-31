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
const metrics = createSignalMetrics("event");
const rate = positiveNumber("FAULTKEEP_EVENT_RPS", "100");

export const options = constantArrivalOptions({
  scenario: "events",
  rate,
  preAllocatedVUs: positiveNumber("FAULTKEEP_EVENT_VUS", "64"),
  durationTrend: "faultkeep_event_duration",
});

export default function sendEvent() {
  const eventId = uniqueId();
  const body = JSON.stringify({
    event_id: eventId,
    timestamp: Date.now() / 1000,
    platform: "javascript",
    level: "error",
    environment: "k6",
    release: "faultkeep-k6-events@1.0.0",
    message: `k6 synthetic error ${eventId}`,
    exception: {
      values: [
        {
          type: "FaultkeepK6Error",
          value: "Synthetic Error Event load fixture",
          stacktrace: { frames: [] },
        },
      ],
    },
    tags: {
      source: "k6",
      scenario: "events-dsn",
    },
  });
  const response = postEnvelope({
    target,
    itemType: "event",
    body,
    eventId,
    tags: { fixture: "faultkeep-event-v1" },
  });
  recordResponse(metrics, response, "Error Event durably accepted");
}
