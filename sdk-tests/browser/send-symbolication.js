import * as Sentry from "@sentry/browser";
import { metricSourceMapHandle } from "../symbolicator/fixtures/source-map.js";

window.__metricSdkResult = { complete: false };
const dsn = new URLSearchParams(window.location.search).get("dsn");
Sentry.init({
  dsn,
  environment: "symbolication-local",
  release: "symbolication-demo@1",
  tracesSampleRate: 0,
  sendDefaultPii: false,
  autoSessionTracking: false,
  sendClientReports: false,
});
let eventId;
try {
  metricSourceMapHandle();
} catch (error) {
  eventId = Sentry.captureException(error);
}
Sentry.flush(8000).then((flushed) => {
  window.__metricSdkResult = {
    complete: true,
    event_id: eventId,
    flushed,
    ...(!flushed || !eventId
      ? { error: "Browser symbolication fixture did not flush" }
      : {}),
  };
});
