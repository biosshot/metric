import * as Sentry from "@sentry/browser";

window.__faultkeepSdkResult = { complete: false };

(async function sendError() {
  try {
    const dsn = new URLSearchParams(window.location.search).get("dsn");
    if (!dsn) {
      throw new Error("Faultkeep DSN query parameter is required");
    }

    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: "sdk-compatibility",
      release: "faultkeep-browser-sdk-test@1.0.0",
      sendDefaultPii: false,
      sendClientReports: false,
      autoSessionTracking: false,
      integrations: [],
    });
    Sentry.setTag("faultkeep.sdk_test", "browser");

    const error = new Error("Faultkeep real Browser SDK compatibility event");
    error.name = "FaultkeepBrowserSdkCompatibilityError";
    const eventId = Sentry.captureException(error);
    const flushed = await Sentry.flush(8_000);
    if (!flushed) {
      throw new Error("the real Browser SDK did not flush the captured Event");
    }
    window.__faultkeepSdkResult = {
      complete: true,
      event_id: eventId,
      flushed,
    };
  } catch (error) {
    window.__faultkeepSdkResult = {
      complete: true,
      error: error instanceof Error ? error.message : String(error),
    };
  }
})();
