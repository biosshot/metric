import * as Sentry from "@sentry/browser";

const status = document.querySelector("#sdk-status");
const statusDetail = document.querySelector("#sdk-status-detail");
const replayIdOutput = document.querySelector("#replay-id");
const eventIdOutput = document.querySelector("#event-id");
const feedbackIdOutput = document.querySelector("#feedback-id");
const flushButton = document.querySelector("#flush-replay");
const captureErrorButton = document.querySelector("#capture-error");
const sendFeedbackButton = document.querySelector("#send-feedback");
const dsn = new URLSearchParams(window.location.search).get("dsn");

function setStatus(title, detail, tone = "ready") {
  status.textContent = title;
  statusDetail.textContent = detail;
  document.body.dataset.sdkStatus = tone;
}

function appendActivity(message) {
  const item = document.createElement("li");
  item.textContent = message;
  document.querySelector("#activity").prepend(item);
}

if (!dsn) {
  setStatus(
    "DSN is missing",
    "Open this page with ?dsn=http://key@localhost:4001/project-id",
    "error",
  );
  flushButton.disabled = true;
  captureErrorButton.disabled = true;
  sendFeedbackButton.disabled = true;
} else {
  Sentry.init({
    dsn,
    tracesSampleRate: 0,
    environment: "manual-replay-demo",
    release: "metric-browser-replay-demo@1.0.0",
    sendDefaultPii: false,
    sendClientReports: false,
    autoSessionTracking: false,
    replaysSessionSampleRate: 1,
    replaysOnErrorSampleRate: 1,
    integrations: [
      Sentry.replayIntegration({
        maskAllText: false,
        maskAllInputs: true,
        blockAllMedia: true,
        mask: [".sentry-mask"],
        block: [".sentry-block"],
        useCompression: true,
      }),
    ],
  });
  const replay = Sentry.getReplay();
  const replayId = replay?.getReplayId(true);
  replayIdOutput.textContent = replayId ?? "not started";
  setStatus(
    replayId ? "Recording" : "Replay unavailable",
    replayId
      ? "Interact with the page, then use “Flush Replay”."
      : "The pinned SDK did not start a Replay session.",
    replayId ? "recording" : "error",
  );
  flushButton.disabled = !replay;
  let stopped = false;

  flushButton.addEventListener("click", async () => {
    flushButton.disabled = true;
    setStatus("Flushing Replay", "Waiting for the SDK transport…", "working");
    try {
      await replay.flush();
      const flushed = await Sentry.flush(8_000);
      if (!flushed) throw new Error("Sentry SDK flush deadline exceeded");
      await replay.stop();
      stopped = true;
      replayIdOutput.textContent =
        replay.getReplayId() ?? replayId ?? "unknown";
      setStatus(
        "Replay sent and stopped",
        "Open Metric → Replays. Reload this page to start another recording.",
        "success",
      );
      appendActivity("Replay segment flushed");
      captureErrorButton.disabled = true;
    } catch (error) {
      setStatus(
        "Replay send failed",
        error instanceof Error ? error.message : String(error),
        "error",
      );
    } finally {
      flushButton.disabled = stopped;
    }
  });

  captureErrorButton.addEventListener("click", async () => {
    const eventId = Sentry.captureException(
      new Error("Metric manual Replay demo error"),
    );
    eventIdOutput.textContent = eventId;
    appendActivity("Test Error captured");
    await Sentry.flush(8_000);
  });

  sendFeedbackButton.addEventListener("click", async () => {
    sendFeedbackButton.disabled = true;
    sendFeedbackButton.textContent = "Sending Feedback…";
    feedbackIdOutput.textContent = "sending";
    try {
      const feedbackId = Sentry.captureFeedback(
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
        throw new Error("Sentry SDK flush deadline exceeded");
      }
      feedbackIdOutput.textContent = feedbackId;
      appendActivity(`Feedback sent: ${feedbackId}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      feedbackIdOutput.textContent = `failed: ${message}`;
      appendActivity(`Feedback send failed: ${message}`);
    } finally {
      sendFeedbackButton.disabled = false;
      sendFeedbackButton.textContent = "Send test Feedback";
    }
  });
}

let count = 0;
const countOutput = document.querySelector("#counter-value");
for (const button of document.querySelectorAll("[data-counter]")) {
  button.addEventListener("click", () => {
    count += Number(button.dataset.counter);
    countOutput.textContent = String(count);
    appendActivity(`Counter changed to ${count}`);
  });
}

for (const tab of document.querySelectorAll("[data-tab]")) {
  tab.addEventListener("click", () => {
    for (const candidate of document.querySelectorAll("[data-tab]")) {
      candidate.classList.toggle("is-active", candidate === tab);
    }
    for (const panel of document.querySelectorAll("[data-panel]")) {
      panel.hidden = panel.dataset.panel !== tab.dataset.tab;
    }
    appendActivity(`Opened ${tab.textContent.trim()} tab`);
  });
}

document.querySelector("#demo-form").addEventListener("submit", (event) => {
  event.preventDefault();
  document.querySelector("#form-result").hidden = false;
  appendActivity("Demo form submitted");
});

document.querySelector("#theme-toggle").addEventListener("click", () => {
  document.body.classList.toggle("demo-light");
  appendActivity("Theme toggled");
});

const modal = document.querySelector("#demo-modal");
document
  .querySelector("#open-modal")
  .addEventListener("click", () => modal.showModal());
document
  .querySelector("#close-modal")
  .addEventListener("click", () => modal.close());

window.__metricReplayDemo = {
  sdk: "@sentry/browser",
  version: "10.66.0",
  active: Boolean(dsn),
};
