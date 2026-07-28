import Player from "rrweb-player";
import "rrweb-player/dist/style.css";

window.__metricSdkResult = { complete: false };

async function decodeRecording(buffer) {
  const raw = new Uint8Array(buffer);
  const newline = raw.indexOf(0x0a);
  if (newline <= 0) throw new Error("retrieved Replay header is malformed");
  const payload = raw.subarray(newline + 1);
  if (payload[0] === 0x5b) return JSON.parse(new TextDecoder().decode(payload));
  const stream = new Blob([payload])
    .stream()
    .pipeThrough(new DecompressionStream("deflate"));
  return JSON.parse(await new Response(stream).text());
}

(async function playReplay() {
  try {
    const response = await fetch("/sdk-replay-recording");
    if (!response.ok)
      throw new Error(`Replay retrieval returned HTTP ${response.status}`);
    const events = await decodeRecording(await response.arrayBuffer());
    if (!Array.isArray(events) || events.length < 2) {
      throw new Error("retrieved Replay has too few rrweb events");
    }
    const target = document.createElement("div");
    target.id = "replay-player";
    document.body.append(target);
    const player = new Player({
      target,
      props: {
        events,
        width: 800,
        height: 450,
        autoPlay: true,
        showController: true,
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 300));
    if (!target.querySelector(".rr-player"))
      throw new Error("rrweb player was not mounted");
    player.pause();
    window.__metricSdkResult = {
      complete: true,
      event_id: "0123456789abcdef0123456789abcdef",
      replay_id: "0123456789abcdef0123456789abcdef",
      event_count: events.length,
      played: true,
      flushed: true,
    };
  } catch (error) {
    window.__metricSdkResult = {
      complete: true,
      error: error instanceof Error ? error.message : String(error),
    };
  }
})();
