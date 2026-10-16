// Serves the game's movies in a format browsers can play.
//
// The IPA's movies use MPEG-4 Part 2 video, which browsers don't decode, so
// each one is extracted from the IPA and transcoded to H.264 with ffmpeg the
// first time it's requested, then cached.
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { spawn } from "node:child_process";

/** Read one file from a ZIP archive whose name ends with `suffix`. */
export function readZipEntry(zipPath, suffix) {
  const zip = fs.readFileSync(zipPath);
  // The end of central directory record is in the last 64 KiB + 22 bytes.
  let eocd = zip.length - 22;
  while (eocd >= 0 && zip.readUInt32LE(eocd) !== 0x06054b50) eocd--;
  if (eocd < 0) throw new Error("Not a ZIP file");
  const count = zip.readUInt16LE(eocd + 10);
  let pos = zip.readUInt32LE(eocd + 16);
  for (let i = 0; i < count; i++) {
    const method = zip.readUInt16LE(pos + 10);
    const compressedSize = zip.readUInt32LE(pos + 20);
    const nameLen = zip.readUInt16LE(pos + 28);
    const extraLen = zip.readUInt16LE(pos + 30);
    const commentLen = zip.readUInt16LE(pos + 32);
    const localOffset = zip.readUInt32LE(pos + 42);
    const name = zip.toString("utf8", pos + 46, pos + 46 + nameLen);
    pos += 46 + nameLen + extraLen + commentLen;
    if (!name.endsWith(suffix)) continue;
    const dataStart =
      localOffset + 30 + zip.readUInt16LE(localOffset + 26) + zip.readUInt16LE(localOffset + 28);
    const data = zip.subarray(dataStart, dataStart + compressedSize);
    if (method === 0) return data;
    if (method === 8) return zlib.inflateRawSync(data);
    throw new Error(`Unsupported ZIP compression method ${method}`);
  }
  return null;
}

function transcode(input, output) {
  return new Promise((resolve, reject) => {
    const tmp = `${output}.tmp.mp4`;
    const ffmpeg = spawn(
      "ffmpeg",
      ["-y", "-loglevel", "error", "-i", "pipe:0", "-c:v", "libx264", "-profile:v", "baseline",
        "-pix_fmt", "yuv420p", "-crf", "20", "-c:a", "aac", "-movflags", "+faststart", tmp],
      { stdio: ["pipe", "ignore", "inherit"] },
    );
    ffmpeg.on("error", reject);
    ffmpeg.on("exit", (code) => {
      if (code !== 0) return reject(new Error(`ffmpeg exited with code ${code}`));
      fs.renameSync(tmp, output);
      resolve();
    });
    ffmpeg.stdin.end(input);
  });
}

export class Movies {
  constructor(ipaPath, cacheDir) {
    this.ipaPath = ipaPath;
    this.cacheDir = cacheDir;
    this.pending = new Map();
  }

  /** Path of a browser-playable copy of `name` (e.g. "intro_v07"), or null. */
  async get(name) {
    if (!/^[\w-]+$/.test(name)) return null;
    const output = path.join(this.cacheDir, `${name}.mp4`);
    if (fs.existsSync(output)) return output;
    if (!this.pending.has(name)) {
      this.pending.set(
        name,
        (async () => {
          const input = readZipEntry(this.ipaPath, `.app/${name}.m4v`);
          if (!input) return null;
          fs.mkdirSync(this.cacheDir, { recursive: true });
          await transcode(input, output);
          return output;
        })().finally(() => this.pending.delete(name)),
      );
    }
    return this.pending.get(name);
  }
}
