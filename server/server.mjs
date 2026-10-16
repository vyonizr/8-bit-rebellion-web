// Serves the web build and the game IPA.
//
// Environment variables:
//   PORT      HTTP port (default 8080)
//   GAME_IPA  path to the 8-Bit Rebellion 1.4.5 IPA (default ../game/8bitrebellion.ipa)
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.resolve(root, "../web");
const ipaPath = path.resolve(process.env.GAME_IPA ?? path.join(root, "../game/8bitrebellion.ipa"));
const port = Number(process.env.PORT ?? 8080);

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

server.listen(port, () => {
  console.log(`Serving on http://localhost:${port}/`);
  if (!fs.existsSync(ipaPath)) console.warn(`Warning: game IPA not found at ${ipaPath}`);
});
