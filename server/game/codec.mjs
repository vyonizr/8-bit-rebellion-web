// Wire format of the 8-Bit Rebellion game protocol. See docs/protocol.md.
//
// A message is a plain object whose keys are the field names from
// server/protocol/messages.json. Missing fields encode as zero, "", false or
// empty, because the client always reads complete structures.
import fs from "node:fs";

const catalogue = JSON.parse(
  fs.readFileSync(new URL("../protocol/messages.json", import.meta.url), "utf8"),
);
const KEY = Buffer.from(catalogue.transport_messages._xor_key, "ascii");

export const COMBINED = "STREAM_COMBINED_MESSAGE";
export const CHANNEL_SETUP = "CHANNEL_SETUP_CONFIRMATION";

function fieldsOf(type) {
  const def = catalogue.messages[type] ?? catalogue.transport_messages[type];
  if (!def) throw new Error(`Unknown message type ${type}`);
  return def.fields;
}

export function xor(buf) {
  const out = Buffer.allocUnsafe(buf.length);
  for (let i = 0; i < buf.length; i++) out[i] = buf[i] ^ KEY[i % KEY.length];
  return out;
}

class Reader {
  constructor(buf) {
    this.buf = buf;
    this.pos = 0;
  }
  int32() {
    const v = this.buf.readInt32BE(this.pos);
    this.pos += 4;
    return v;
  }
  bool() {
    return this.buf[this.pos++] !== 0;
  }
  bytes() {
    const len = this.buf.readUInt32BE(this.pos);
    this.pos += 4;
    const v = this.buf.subarray(this.pos, this.pos + len);
    if (v.length !== len) throw new Error("Truncated message");
    this.pos += len;
    return Buffer.from(v);
  }
  string() {
    return this.bytes().toString("utf8");
  }
  rest() {
    return this.buf.subarray(this.pos);
  }
}

class Writer {
  constructor() {
    this.parts = [];
  }
  int32(v) {
    const b = Buffer.alloc(4);
    b.writeInt32BE(v | 0);
    this.parts.push(b);
  }
  bool(v) {
    this.parts.push(Buffer.from([v ? 1 : 0]));
  }
  bytes(v) {
    const data = Buffer.from(v ?? []);
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    this.parts.push(len, data);
  }
  string(v) {
    this.bytes(Buffer.from(String(v ?? ""), "utf8"));
  }
  result() {
    return Buffer.concat(this.parts);
  }
}

function readValue(r, type, of) {
  if (type === "int32") return r.int32();
  if (type === "bool") return r.bool();
  if (type === "string" || type === "datetime") return r.string();
  if (type === "bytes") return r.bytes();
  if (type === "array") {
    const n = r.int32();
    return Array.from({ length: n }, () => readValue(r, of));
  }
  if (type.startsWith("entity:")) return readFields(r, catalogue.entities[type.slice(7)].fields);
  throw new Error(`Unknown field type ${type}`);
}

function readFields(r, fields) {
  const obj = {};
  for (const f of fields) obj[f.name] = readValue(r, f.type, f.of);
  return obj;
}

function writeValue(w, type, of, v) {
  if (type === "int32") w.int32(v ?? 0);
  else if (type === "bool") w.bool(v);
  else if (type === "string" || type === "datetime") w.string(v);
  else if (type === "bytes") w.bytes(v);
  else if (type === "array") {
    const items = v ?? [];
    w.int32(items.length);
    for (const item of items) writeValue(w, of, undefined, item);
  } else if (type.startsWith("entity:")) writeFields(w, catalogue.entities[type.slice(7)].fields, v ?? {});
  else throw new Error(`Unknown field type ${type}`);
}

function writeFields(w, fields, obj) {
  for (const f of fields) writeValue(w, f.type, f.of, obj[f.name]);
}

export function decodeBody(type, body) {
  return readFields(new Reader(body), fieldsOf(type));
}

export function encodeBody(type, msg) {
  const w = new Writer();
  writeFields(w, fieldsOf(type), msg);
  return w.result();
}

/** Encode one message as a PDU. */
export function encodePdu(type, sessionId, msg) {
  const w = new Writer();
  w.string(type);
  w.string(sessionId);
  const payload = xor(Buffer.concat([w.result(), encodeBody(type, msg)]));
  const header = Buffer.from([0x02, 0x00, 0, 0, 0, 0]);
  header.writeUInt32BE(payload.length, 2);
  return Buffer.concat([header, payload]);
}

/**
 * Decode one PDU into { sessionId, messages: [{ type, msg }] }, unpacking
 * combined messages.
 */
export function decodePdu(buf) {
  if (buf.length < 6) throw new Error("Truncated PDU");
  const len = buf.readUInt32BE(2);
  const r = new Reader(xor(buf.subarray(6, 6 + len)));
  const type = r.string();
  const sessionId = r.string();
  if (type !== COMBINED) return { sessionId, messages: [{ type, msg: decodeBody(type, r.rest()) }] };
  const messages = [];
  const count = r.int32();
  for (let i = 0; i < count; i++) {
    const item = new Reader(r.bytes());
    const itemType = item.string();
    messages.push({ type: itemType, msg: decodeBody(itemType, item.rest()) });
  }
  return { sessionId, messages };
}

/** Encode messages from the client side, for tests. */
export function encodeCombinedPdu(sessionId, messages) {
  const w = new Writer();
  w.int32(messages.length);
  for (const { type, msg } of messages) {
    const item = new Writer();
    item.string(type);
    w.bytes(Buffer.concat([item.result(), encodeBody(type, msg)]));
  }
  const head = new Writer();
  head.string(COMBINED);
  head.string(sessionId);
  const payload = xor(Buffer.concat([head.result(), w.result()]));
  const header = Buffer.from([0x02, 0x00, 0, 0, 0, 0]);
  header.writeUInt32BE(payload.length, 2);
  return Buffer.concat([header, payload]);
}

/** The body of a poll or send response: a count, then length-prefixed PDUs. */
export function encodeContainer(pdus) {
  const w = new Writer();
  w.int32(pdus.length);
  for (const pdu of pdus) w.bytes(pdu);
  return w.result();
}

export function decodeContainer(buf) {
  if (buf.length === 0) return [];
  const r = new Reader(buf);
  const n = r.int32();
  return Array.from({ length: n }, () => r.bytes());
}

/** Format a date the way the client parses it: YYYYMMDDhhmmss. */
export function formatDate(date = new Date()) {
  const p = (n, w = 2) => String(n).padStart(w, "0");
  return (
    p(date.getFullYear(), 4) +
    p(date.getMonth() + 1) +
    p(date.getDate()) +
    p(date.getHours()) +
    p(date.getMinutes()) +
    p(date.getSeconds())
  );
}
