// Loads touchHLE (WebAssembly) and the game, then starts it on a click.
// A click is required anyway: browsers only allow audio after a user gesture.
"use strict";

const IPA_URL = "game/8bitrebellion.ipa";
const IPA_PATH = "/8bitrebellion.ipa";
// touchHLE keeps each app's Documents directory here; IndexedDB persists it.
const SAVE_DIR = "/touchHLE_sandbox";
// Same options as 8BeetBebellion's launcher.
const ARGS = [
  "--landscape-right",
  "--landscape-content-layout",
  "--keyboard-game-controls",
  "--tolerate-nil-dictionary-keys",
  "--no-error-popup",
];

const canvas = document.getElementById("canvas");
const overlay = document.getElementById("overlay");
const startButton = document.getElementById("start");
const statusText = document.getElementById("status");

// The emulator's log, kept for debugging (window.touchhleLog).
const logLines = (window.touchhleLog = []);
function log(text) {
  logLines.push(text);
  if (logLines.length > 5000) logLines.splice(0, 1000);
  console.log(text);
}

function setStatus(text) {
  statusText.textContent = text;
}

async function fetchWithProgress(url, label) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${label}: HTTP ${response.status}`);
  const total = Number(response.headers.get("Content-Length")) || 0;
  const reader = response.body.getReader();
  const chunks = [];
  let received = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    received += value.length;
    const mb = (received / 1048576).toFixed(0);
    setStatus(total ? `${label}: ${Math.floor((received / total) * 100)}%` : `${label}: ${mb} MB`);
  }
  const data = new Uint8Array(received);
  let offset = 0;
  for (const chunk of chunks) {
    data.set(chunk, offset);
    offset += chunk.length;
  }
  return data;
}

function syncfs(module, populate) {
  return new Promise((resolve, reject) =>
    module.FS.syncfs(populate, (err) => (err ? reject(err) : resolve())),
  );
}

// Writes saves to IndexedDB, skipping a sync if one is still running.
function makePersister(module) {
  let busy = false;
  return () => {
    if (busy) return;
    busy = true;
    syncfs(module, false)
      .catch((err) => console.error("Saving failed:", err))
      .finally(() => (busy = false));
  };
}

async function main() {
  setStatus("Loading…");
  const [module, ipa] = await Promise.all([
    createTouchHLE({
      canvas,
      locateFile: (file) => `dist/${file}`,
      print: log,
      printErr: log,
      onAbort: (reason) => {
        overlay.hidden = false;
        startButton.hidden = true;
        setStatus(`The emulator stopped: ${reason}. Reload the page to restart.`);
      },
    }),
    fetchWithProgress(IPA_URL, "Downloading game"),
  ]);

  module.FS.mkdir(SAVE_DIR);
  module.FS.mount(module.IDBFS, {}, SAVE_DIR);
  await syncfs(module, true);
  module.FS.writeFile(IPA_PATH, ipa);

  setStatus("");
  startButton.textContent = "Play";
  startButton.disabled = false;
  startButton.focus();
  startButton.addEventListener(
    "click",
    () => {
      overlay.hidden = true;
      canvas.focus();
      const persist = makePersister(module);
      setInterval(persist, 5000);
      addEventListener("pagehide", persist);
      module.callMain([IPA_PATH, ...ARGS]);
    },
    { once: true },
  );
  // For automated testing. Audio stays muted until the page gets a click.
  if (new URLSearchParams(location.search).has("autostart")) startButton.click();
}

main().catch((err) => {
  console.error(err);
  startButton.hidden = true;
  setStatus(`Couldn't start: ${err.message}`);
});
