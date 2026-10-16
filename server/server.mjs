// Serves the web build, the game IPA and the replacement game server.
//
// Environment variables:
//   PORT      HTTP port (default 8080)
//   GAME_IPA  path to the 8-Bit Rebellion 1.4.5 IPA (default ../game/8bitrebellion.ipa)
//   GAME_DB   path to the game database (default data/game.sqlite)
//   GAME_DEBUG  set to log every game message
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { WebSocketServer, createWebSocketStream } from "ws";
import { Store } from "./game/store.mjs";
import { Game } from "./game/game.mjs";

const root = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.resolve(root, "../web");
const ipaPath = path.resolve(process.env.GAME_IPA ?? path.join(root, "../game/8bitrebellion.ipa"));
const port = Number(process.env.PORT ?? 8080);
const dbPath = path.resolve(process.env.GAME_DB ?? path.join(root, "data/game.sqlite"));
fs.mkdirSync(path.dirname(dbPath), { recursive: true });
const game = new Game(new Store(dbPath), { trace: !!process.env.GAME_DEBUG });

/** Largest request body accepted from the game (one batch of messages). */
const MAX_BODY = 1 << 20;

function handleGame(req, res) {
  const chunks = [];
  let size = 0;
  req.on("data", (chunk) => {
    size += chunk.length;
    if (size > MAX_BODY) req.destroy();
    else chunks.push(chunk);
  });
  req.on("end", () => game.handleHttp(req, res, Buffer.concat(chunks)));
}

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".data": "application/octet-stream",
  ".ipa": "application/octet-stream",
};

function sendFile(res, file) {
  fs.stat(file, (err, stat) => {
    if (err || !stat.isFile()) {
      res.writeHead(404).end("Not found");
      return;
    }
    res.writeHead(200, {
      "Content-Type": TYPES[path.extname(file)] ?? "application/octet-stream",
      "Content-Length": stat.size,
      "Cache-Control": "no-cache",
    });
    fs.createReadStream(file).pipe(res);
  });
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  // The game asks for http://lp110.m-botme.com/lp/httpGDServlet.
  if (url.pathname === "/lp/httpGDServlet") {
    handleGame(req, res);
    return;
  }
  if (url.pathname === "/game/8bitrebellion.ipa") {
    sendFile(res, ipaPath);
    return;
  }
  const rel = url.pathname === "/" ? "index.html" : decodeURIComponent(url.pathname.slice(1));
  const file = path.resolve(webDir, rel);
  // Refuse paths that escape web/.
  if (!file.startsWith(webDir + path.sep)) {
    res.writeHead(403).end("Forbidden");
    return;
  }
  sendFile(res, file);
});

// The game's own HTTP client talks raw TCP. In the browser, touchHLE tunnels
// each TCP connection through a WebSocket at /net; here the WebSocket becomes
// a stream that this same HTTP server reads requests from.
const wss = new WebSocketServer({ noServer: true });
server.on("upgrade", (req, socket, head) => {
  if (new URL(req.url, "http://localhost").pathname !== "/net") {
    socket.destroy();
    return;
  }
  wss.handleUpgrade(req, socket, head, (ws) => {
    server.emit("connection", createWebSocketStream(ws));
  });
});

server.listen(port, () => {
  console.log(`Serving on http://localhost:${port}/`);
  if (!fs.existsSync(ipaPath)) console.warn(`Warning: game IPA not found at ${ipaPath}`);
});
