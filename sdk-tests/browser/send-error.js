import * as Sentry from "@sentry/browser";

window.__metricSdkResult = { complete: false };

(async function sendError() {
  try {
    const dsn = new URLSearchParams(window.location.search).get("dsn");
    if (!dsn) {
      throw new Error("Metric DSN query parameter is required");
    }

    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: "sdk-compatibility",
      release: "metric-browser-sdk-test@1.0.0",
      sendDefaultPii: false,
      sendClientReports: false,
      autoSessionTracking: false,
      integrations: [],
    });
    Sentry.setTag("metric.sdk_test", "browser");

    const error = new Error("Metric real Browser SDK compatibility event");
    error.name = "MetricBrowserSdkCompatibilityError";
    const eventId = Sentry.captureException(error);
    const flushed = await Sentry.flush(8_000);
    if (!flushed) {
      throw new Error("the real Browser SDK did not flush the captured Event");
    }
    window.__metricSdkResult = {
      complete: true,
      event_id: eventId,
      flushed,
    };
  } catch (error) {
    window.__metricSdkResult = {
      complete: true,
      error: error instanceof Error ? error.message : String(error),
    };
  }
})();
