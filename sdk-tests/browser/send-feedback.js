import * as Sentry from "@sentry/browser";

window.__metricSdkResult = { complete: false };

(async function sendFeedback() {
  try {
    const dsn = new URLSearchParams(window.location.search).get("dsn");
    if (!dsn) {
      throw new Error("Metric DSN query parameter is required");
    }

    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: "sdk-compatibility",
      release: "metric-browser-feedback-test@1.0.0",
      sendDefaultPii: false,
      sendClientReports: false,
      autoSessionTracking: false,
      integrations: [],
    });
    const eventId = Sentry.captureFeedback(
      {
        message: "The checkout button did not respond",
        name: "Ada",
        email: "ada@example.com",
        url: "https://example.test/checkout",
        source: "metric-sdk-compatibility",
      },
      {
        attachments: [
          {
            filename: "feedback-context.txt",
            data: "safe browser feedback context",
            contentType: "text/plain",
          },
        ],
      },
    );
    const flushed = await Sentry.flush(8_000);
    if (!flushed) {
      throw new Error("the real Browser SDK did not flush Feedback");
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
