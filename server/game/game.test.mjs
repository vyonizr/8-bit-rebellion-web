// Run with: node --test server/game/game.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import {
  CHANNEL_SETUP,
  decodeBody,
  decodeContainer,
  decodePdu,
  encodeBody,
  encodeCombinedPdu,
} from "./codec.mjs";
import { Store } from "./store.mjs";
import { Game } from "./game.mjs";

const catalogue = JSON.parse(fs.readFileSync(new URL("../protocol/messages.json", import.meta.url)));

test("every message survives an encode/decode round trip", () => {
  for (const type of Object.keys(catalogue.messages)) {
    const decoded = decodeBody(type, encodeBody(type, {}));
    assert.deepEqual(encodeBody(type, decoded), encodeBody(type, {}), type);
  }
});

/** Plays the client side of the protocol against a Game, without HTTP. */
class FakeClient {
  constructor(game) {
    this.game = game;
    this.sessionId = "";
    this.playerId = 0;
    this.inbox = [];
    const res = this.request("GET", { action: "INIT_CHANNEL" });
    this.cookie = /JSESSIONID=([0-9a-f]+)/.exec(res.headers["Set-Cookie"])[1];
    this.poll();
    const setup = this.take(CHANNEL_SETUP);
    assert.notEqual(setup.channelStatus, 0);
    assert.ok(this.sessionId);
  }

  request(method, headers, body = Buffer.alloc(0)) {
    const res = {
      writeHead: (status, h) => Object.assign(res, { status, headers: h }),
      end: (data) => (res.body = data ?? Buffer.alloc(0)),
    };
    this.game.handleHttp({ method, headers }, res, body);
    return res;
  }

  receive(res) {
    assert.equal(res.headers.SESSION_VALID, "TRUE");
    for (const pdu of decodeContainer(res.body)) {
      const { sessionId, messages } = decodePdu(pdu);
      this.sessionId = sessionId;
      this.inbox.push(...messages);
    }
  }

  poll() {
    this.receive(this.request("GET", { cookie: `JSESSIONID=${this.cookie}`, pooling: "TRUE" }));
  }

  send(type, msg = {}) {
    const pdu = encodeCombinedPdu(this.sessionId, [{ type, msg: { playerID: this.playerId, ...msg } }]);
    this.receive(this.request("POST", { cookie: `JSESSIONID=${this.cookie}` }, pdu));
  }

  /** Remove and return the first received message of a type. */
  take(type) {
    const i = this.inbox.findIndex((m) => m.type === type);
    assert.ok(i >= 0, `expected ${type}, got ${this.inbox.map((m) => m.type).join(", ")}`);
    return this.inbox.splice(i, 1)[0].msg;
  }

  register(name) {
    this.send("CREATE_ACCOUNT", { account: { userName: name, password: "pw", email: `${name}@x` } });
    assert.equal(this.take("CREATE_ACCOUNT_RESPONSE").responseCode, 0);
    this.send("LOGIN", { userName: name, password: "pw", version: 1 });
    assert.equal(this.take("LOGIN_RESPONSE").responseCode, 4);
    this.send("CREATE_PROFILE", { profile: { profileName: name, status: 3 } });
    const { responseCode, playerProfile } = this.take("CREATE_PROFILE_RESPONSE");
    assert.equal(responseCode, 0);
    this.playerId = playerProfile.playerID;
    return playerProfile;
  }
}

test("guest login keeps the local profile and reuses the guest", () => {
  const game = new Game(new Store(":memory:"), { log: () => {} });
  const a = new FakeClient(game);
  a.send("GUEST_LOGIN", { version: 1 });
  const res = a.take("LOGIN_RESPONSE");
  assert.equal(res.responseCode, 5);
  assert.ok(res.playerID > 0);

  const again = new FakeClient(game);
  again.playerId = res.playerID;
  again.send("GUEST_LOGIN", { version: 1 });
  assert.equal(again.take("LOGIN_RESPONSE").playerID, res.playerID);
});

test("unknown sessions are told to reconnect", () => {
  const game = new Game(new Store(":memory:"), { log: () => {} });
  const res = new FakeClient(game).request("GET", { cookie: "JSESSIONID=00" });
  assert.equal(res.headers.SESSION_VALID, "FALSE");
});

test("accounts, login and presence", () => {
  const game = new Game(new Store(":memory:"), { log: () => {} });
  const a = new FakeClient(game);
  const profile = a.register("alice");
  assert.equal(profile.profileName, "alice");

  const b = new FakeClient(game);
  b.send("CREATE_ACCOUNT", { account: { userName: "ALICE", password: "x", email: "y" } });
  assert.equal(b.take("CREATE_ACCOUNT_RESPONSE").responseCode, 1);
  b.send("LOGIN", { userName: "alice", password: "wrong", version: 1 });
  assert.equal(b.take("LOGIN_RESPONSE").responseCode, 1);
  b.register("bob");

  a.send("CHANGE_LOCATION", { locationID: 3 });
  assert.deepEqual(a.take("LOCATION_PLAYER_PROFILE").playerProfileList, []);
  b.send("CHANGE_LOCATION", { locationID: 3 });
  assert.equal(b.take("LOCATION_PLAYER_PROFILE").playerProfileList[0].profileName, "alice");
  a.poll();
  assert.equal(a.take("LOCATION_PLAYER_PROFILE").playerProfileList[0].profileName, "bob");

  // Logging in again (names are case-insensitive) returns the stored profile.
  const c = new FakeClient(game);
  c.send("LOGIN", { userName: "ALICE", password: "pw", version: 1 });
  const login = c.take("LOGIN_RESPONSE");
  assert.equal(login.responseCode, 0);
  assert.equal(login.playerID, a.playerId);
  assert.equal(login.profile.profileName, "alice");
});

test("friends, chat and mail between two players", () => {
  const game = new Game(new Store(":memory:"), { log: () => {} });
  const a = new FakeClient(game);
  a.register("alice");
  const b = new FakeClient(game);
  b.register("bob");

  // Friend request and acceptance.
  a.send("SEND_REQUEST", { request: { targetPlayerID: b.playerId, type: 1 } });
  b.poll();
  const [request] = b.take("REQUEST_LIST").rList;
  assert.equal(request.fromPlayerProfile.profileName, "alice");
  b.send("RETURN_RESPONSE", { requestID: request.requestID, requestType: 1, responseCode: 0, targetPlayerID: a.playerId });
  assert.equal(b.take("NEW_FRIEND_MADE").friendProfile.profileName, "alice");
  a.poll();
  assert.equal(a.take("NEW_FRIEND_MADE").friendProfile.profileName, "bob");

  // Chat.
  a.send("SEND_REQUEST", { request: { targetPlayerID: b.playerId, type: 0, isInstance: true } });
  b.poll();
  const chat = b.take("REQUEST_LIST").rList[0];
  b.send("RETURN_RESPONSE", { requestID: chat.requestID, requestType: 0, responseCode: 0, targetPlayerID: a.playerId });
  assert.equal(b.take("IM_START").targetPlayerProfile.profileName, "alice");
  a.send("IM_MESSAGE", { targetPlayerID: b.playerId, message: Buffer.from("hi") });
  assert.equal(a.take("IM_START").targetPlayerProfile.profileName, "bob");
  b.poll();
  const im = b.take("IM_MESSAGE");
  assert.equal(im.targetPlayerID, a.playerId);
  assert.equal(im.message.toString(), "hi");

  // Mail.
  a.send("SEND_MAIL", { targetPlayerID: b.playerId, message: Buffer.from("hello"), subject: "yo" });
  assert.equal(a.take("SEND_MAIL_RESPONSE").responseCode, 0);
  b.poll();
  assert.equal(b.take("NEW_MAIL").mail.senderName, "alice");
  b.send("GET_ALL_MAIL");
  assert.equal(b.take("MAIL_LIST").mailList.length, 1);

  // Items survive in the item list.
  a.send("ADD_ITEM", { item: { tempPlayerItemID: 7, itemID: 17001, itemType: 17 } });
  const added = a.take("ADD_ITEM_RESPONSE").item;
  assert.equal(added.tempPlayerItemID, 7);
  a.send("GET_ALL_PLAYER_ITEMS");
  assert.equal(a.take("ITEM_LIST").itemList[0].realPlayerItemID, added.realPlayerItemID);
});
