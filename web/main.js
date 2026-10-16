// Loads touchHLE (WebAssembly) and the game, then starts it on a click.
// A click is required anyway: browsers only allow audio after a user gesture.
//
// The game file (IPA) comes from one of two places:
// - Server-hosted: the server has the IPA and sends it (private servers).
// - Player-supplied: the server has no IPA, so the player chooses their own
//   copy. It stays in the browser (IndexedDB). Only its movies are sent to
//   the server, which converts them for playback without keeping them.
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
const chooseButton = document.getElementById("choose");
const fileInput = document.getElementById("file");
const forgetButton = document.getElementById("forget");
const helpText = document.getElementById("help");
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

// The player's own IPA, kept in IndexedDB so they choose it only once.
function withIpaStore(mode, action) {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open("8br-game", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("files");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const db = open.result;
      const request = action(db.transaction("files", mode).objectStore("files"));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
      request.transaction.oncomplete = () => db.close();
    };
  });
}
const loadCachedIpa = () => withIpaStore("readonly", (store) => store.get("ipa"));
const cacheIpa = (data) => withIpaStore("readwrite", (store) => store.put(data, "ipa"));
const forgetIpa = () => withIpaStore("readwrite", (store) => store.delete("ipa"));

/** Ask the player to choose their IPA. */
function chooseIpa() {
  helpText.hidden = false;
  chooseButton.hidden = false;
  setStatus("");
  chooseButton.onclick = () => fileInput.click();
  chooseButton.focus();
  return new Promise((resolve) => {
    fileInput.onchange = async () => {
      const file = fileInput.files[0];
      fileInput.value = "";
      if (!file) return;
      const data = new Uint8Array(await file.arrayBuffer());
      // IPA files are ZIP archives.
      if (data[0] !== 0x50 || data[1] !== 0x4b || data[2] !== 3 || data[3] !== 4) {
        setStatus(`${file.name} isn't an IPA file. Choose the game's .ipa file.`);
        return;
      }
      helpText.hidden = true;
      chooseButton.hidden = true;
      resolve(data);
    };
  });
}

async function getIpa(hosted) {
  if (hosted) return fetchWithProgress(IPA_URL, "Downloading game");
  let data = await loadCachedIpa().catch(() => null);
  if (!data) {
    data = await chooseIpa();
    setStatus("Saving the game file in this browser…");
    await cacheIpa(data).catch((err) => console.warn("Couldn't keep the game file:", err));
  }
  forgetButton.hidden = false;
  return new Uint8Array(data);
}

/**
 * URL of a browser-playable copy of a movie. The originals use MPEG-4 Part 2
 * video, which browsers can't decode, so the server converts them.
 */
function makeMovieUrlProvider(hosted) {
  const converted = new Map();
  return (name, original) => {
    if (hosted) return `game/movies/${name}.mp4`;
    if (!converted.has(name)) {
      const url = fetch("game/movies/convert", { method: "POST", body: original })
        .then((response) => {
          if (!response.ok) throw new Error(`Movie conversion failed: HTTP ${response.status}`);
          return response.blob();
        })
        .then((blob) => URL.createObjectURL(blob));
      // Retry on the next attempt if this one fails.
      url.catch(() => converted.delete(name));
      converted.set(name, url);
    }
    return converted.get(name);
  };
}

// Game size. touchHLE renders at the iPhone's 480x320. The default shows one
// game pixel per CSS pixel; players can pick 2x, 3x or fitting the window.
const GAME_W = 480;
const GAME_H = 320;
const SIZE_KEY = "8br-size";
const stage = document.getElementById("stage");
const toolbar = document.getElementById("toolbar");
let gameSize = "1";
try {
  gameSize = localStorage.getItem(SIZE_KEY) ?? "1";
} catch {}

function layoutGame() {
  const fit = Math.min(stage.clientWidth / GAME_W, stage.clientHeight / GAME_H);
  let scale = fit;
  if (gameSize !== "fit" && !document.fullscreenElement) {
    // Use the largest whole-number size that fits, or fit if even 1x doesn't.
    scale = Math.min(Number(gameSize), Math.floor(fit)) || fit;
  }
  stage.style.setProperty("--game-w", `${GAME_W * scale}px`);
  stage.style.setProperty("--game-h", `${GAME_H * scale}px`);
  // Keep pixels crisp only when each game pixel covers a whole number of
  // screen pixels; otherwise they'd be uneven, so smooth them instead.
  const screenScale = scale * devicePixelRatio;
  stage.classList.toggle("smooth", Math.abs(screenScale - Math.round(screenScale)) > 0.01);
  for (const button of toolbar.querySelectorAll("[data-size]")) {
    button.setAttribute("aria-pressed", String(button.dataset.size === gameSize));
  }
}

function setUpSizeControls() {
  for (const button of toolbar.querySelectorAll("[data-size]")) {
    button.addEventListener("click", () => {
      gameSize = button.dataset.size;
      try {
        localStorage.setItem(SIZE_KEY, gameSize);
      } catch {}
      layoutGame();
      canvas.focus();
    });
  }
  document.getElementById("fullscreen").addEventListener("click", async () => {
    await document.documentElement.requestFullscreen().catch(() => {});
    canvas.focus();
  });
  addEventListener("resize", layoutGame);
  document.addEventListener("fullscreenchange", layoutGame);
  layoutGame();
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
  setUpSizeControls();
  setStatus("Loading…");
  forgetButton.addEventListener("click", async () => {
    await forgetIpa();
    location.reload();
  });
  const hosted = (await fetch(IPA_URL, { method: "HEAD" })).ok;
  const [module, ipa] = await Promise.all([
    createTouchHLE({
      canvas,
      locateFile: (file) => `dist/${file}`,
      print: log,
      printErr: log,
      touchhleMovieUrl: makeMovieUrlProvider(hosted),
      onAbort: (reason) => {
        overlay.hidden = false;
        startButton.hidden = true;
        setStatus(`The emulator stopped: ${reason}. Reload the page to restart.`);
      },
    }),
    getIpa(hosted),
  ]);

  module.FS.mkdir(SAVE_DIR);
  module.FS.mount(module.IDBFS, {}, SAVE_DIR);
  await syncfs(module, true);
  module.FS.writeFile(IPA_PATH, ipa);

  setStatus("");
  startButton.hidden = false;
  startButton.focus();
  startButton.addEventListener(
    "click",
    () => {
      overlay.hidden = true;
      toolbar.hidden = false;
      layoutGame();
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
