// Serves the web build, the game IPA and the replacement game server.
//
// Environment variables:
//   PORT      HTTP port (default 8080)
//   GAME_IPA  path to the 8-Bit Rebellion 1.4.5 IPA to host (default
//             ../game/8bitrebellion.ipa). Without one, players supply their own.
//   GAME_DB   path to the game database (default data/game.sqlite)
//   GAME_DEBUG  set to log every game message
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { WebSocketServer, createWebSocketStream } from "ws";
import { Store } from "./game/store.mjs";
import { Game } from "./game/game.mjs";
import { MAX_UPLOAD, Movies, convertUpload } from "./movies.mjs";

const root = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.resolve(root, "../web");
const ipaPath = path.resolve(process.env.GAME_IPA ?? path.join(root, "../game/8bitrebellion.ipa"));
const port = Number(process.env.PORT ?? 8080);
const dbPath = path.resolve(process.env.GAME_DB ?? path.join(root, "data/game.sqlite"));
fs.mkdirSync(path.dirname(dbPath), { recursive: true });
const movies = new Movies(ipaPath, path.join(path.dirname(dbPath), "movies"));
const game = new Game(new Store(dbPath), { trace: !!process.env.GAME_DEBUG });

/** Largest request body accepted from the game (one batch of messages). */
const MAX_BODY = 1 << 20;

/** Read a request body of at most `limit` bytes, then call `done` with it. */
function readBody(req, limit, done) {
  const chunks = [];
  let size = 0;
  req.on("data", (chunk) => {
    size += chunk.length;
    if (size > limit) req.destroy();
    else chunks.push(chunk);
  });
  req.on("end", () => done(Buffer.concat(chunks)));
}

function handleGame(req, res) {
  readBody(req, MAX_BODY, (body) => game.handleHttp(req, res, body));
}

// Player-supplied mode: the page sends a movie from the player's own IPA.
function handleMovieConversion(req, res) {
  if (req.method !== "POST") {
    res.writeHead(405).end();
    return;
  }
  readBody(req, MAX_UPLOAD, (body) => {
    convertUpload(body).then(
      (mp4) => {
        if (!mp4) return res.writeHead(503, { "Retry-After": "5" }).end("Busy");
        res.writeHead(200, { "Content-Type": "video/mp4", "Content-Length": mp4.length }).end(mp4);
      },
      (err) => {
        console.error(`Movie conversion failed: ${err.message}`);
        res.writeHead(422).end("Couldn't convert this movie");
      },
    );
  });
}

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".data": "application/octet-stream",
  ".ipa": "application/octet-stream",
  ".mp4": "video/mp4",
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
  if (url.pathname === "/game/movies/convert") {
    handleMovieConversion(req, res);
    return;
  }
  const movie = /^\/game\/movies\/([\w-]+)\.mp4$/.exec(url.pathname);
  if (movie) {
    movies.get(movie[1]).then(
      (file) => (file ? sendFile(res, file) : res.writeHead(404).end("Not found")),
      (err) => {
        console.error(`Couldn't prepare movie ${movie[1]}: ${err.message}`);
        res.writeHead(500).end("Movie unavailable");
      },
    );
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
  if (fs.existsSync(ipaPath)) {
    console.log(`Hosting the game from ${ipaPath}. Only do this on a private server.`);
  } else {
    console.log(`No game IPA at ${ipaPath}: players choose their own copy.`);
  }
});
