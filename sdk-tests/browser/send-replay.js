import * as Sentry from "@sentry/browser";

window.__metricSdkResult = { complete: false };

(async function sendReplay() {
  try {
    const dsn = new URLSearchParams(window.location.search).get("dsn");
    if (!dsn) throw new Error("Metric DSN query parameter is required");

    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: "sdk-compatibility",
      release: "metric-browser-replay-test@1.0.0",
      sendDefaultPii: false,
      sendClientReports: false,
      autoSessionTracking: false,
      replaysSessionSampleRate: 1,
      replaysOnErrorSampleRate: 0,
      integrations: [
        Sentry.replayIntegration({
          maskAllText: true,
          maskAllInputs: true,
          blockAllMedia: true,
          minReplayDuration: 0,
          flushMinDelay: 0,
          flushMaxDelay: 0,
          useCompression: true,
        }),
      ],
    });

    const secret = document.createElement("input");
    secret.value = "top-secret-value";
    secret.setAttribute("aria-label", "masked test input");
    document.querySelector("main")?.append(secret);
    secret.focus();
    secret.value = "top-secret-value-updated";
    secret.dispatchEvent(new Event("input", { bubbles: true }));
    document.body.dataset.replayInteraction = "complete";
    await new Promise((resolve) => setTimeout(resolve, 150));

    const replay = Sentry.getReplay();
    const replayId = replay?.getReplayId(true);
    if (!replay || !replayId)
      throw new Error("the pinned SDK did not start Replay");
    await replay.flush();
    const flushed = await Sentry.flush(8_000);
    await replay.stop();
    if (!flushed) throw new Error("the pinned SDK did not flush Replay");
    window.__metricSdkResult = {
      complete: true,
      event_id: replayId,
      replay_id: replayId,
      flushed,
    };
  } catch (error) {
    window.__metricSdkResult = {
      complete: true,
      error: error instanceof Error ? error.message : String(error),
    };
  }
})();
