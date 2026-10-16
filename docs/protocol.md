# 8-Bit Rebellion online protocol

This document specifies the client/server protocol of *Linkin Park 8-Bit Rebellion!* 1.4.5 (`com.alife.linkinpark`, Artificial Life Inc., 2010), recovered by static analysis of the game binary. It is the reference for writing a replacement server. The machine-readable catalogue of every message and entity is [`server/protocol/messages.json`](../server/protocol/messages.json).

## Summary

- **Transport:** plain HTTP/1.1 over TCP to `http://lp110.m-botme.com/lp/httpGDServlet` (port 80). The URL is hard-coded. There is no TLS and no server list lookup. The client polls with `GET` and sends with `POST`; the server never pushes on its own connection.
- **Session:** a `JSESSIONID` cookie set by the server, plus a session ID string that the server assigns in a binary handshake message.
- **Framing:** every message travels in a 6-byte PDU header (`0x02 0x00` + big-endian length) followed by a payload XOR-obfuscated with the ASCII key `LNKINPARK_ENCRYPTOR_KEY_ALIFE`.
- **Message identity:** messages are identified by an ASCII type name such as `LOGIN` or `LOGIN_RESPONSE`, not by a number.
- **Encoding:** big-endian `int32`, 1-byte `bool`, and length-prefixed strings and blobs. Field order is fixed per message class and hand-written (code-generated) in `LPServerMessageParser`.
- **Catalogue:** 130 game messages and 27 entities, plus 2 transport messages. The wire layout of all of them is fully specified: both the encoder and the decoder of each class were extracted and agree field for field. Client behaviour is specified for the 87 messages the client actually sends or handles, and partly for 1 more (`PRODUCT_LIST`). The other 42 are compiled in but never sent or handled by this client version (crew battle, missions, fight results, media, NPC content, options, logs).
- **Authority:** the client is authoritative for currency, items, missions, fights and achievements. The server stores what the client reports (`PLAYER_PROFILE_UPDATE`, `ADD_ITEM`, `UPDATE_APARTMENT_DATA`) and relays social traffic (friends, IM, mail, bulletin, presence).

Evidence notation: function names are Objective-C methods in the armv7 slice; addresses are armv7 virtual addresses. **Observed** means read in decompiled code. **Inferred** means deduced from naming or surrounding code but not proven by a code path.

## Contents

1. [Endpoints](#endpoints)
2. [HTTP layer](#http-layer)
3. [PDU and payload framing](#pdu-and-payload-framing)
4. [Primitive encoding](#primitive-encoding)
5. [Connection handshake and session](#connection-handshake-and-session)
6. [Client send loop and batching](#client-send-loop-and-batching)
7. [Catalogue schema (`messages.json`)](#catalogue-schema-messagesjson)
8. [Flows](#flows)
9. [Message catalogue](#message-catalogue)
10. [Server-provided data and local seed data](#server-provided-data-and-local-seed-data)
11. [HTTP side channels](#http-side-channels)
12. [Game-side patches and emulator requirements](#game-side-patches-and-emulator-requirements)
13. [Open questions](#open-questions)
14. [Reproducing the analysis](#reproducing-the-analysis)

## Endpoints

| Use | Address | Source | Status |
|---|---|---|---|
| Game server (all gameplay messages) | `http://lp110.m-botme.com/lp/httpGDServlet` | `+[NetworkHandler startStreamNetworkManager]` @ `0x1a36e4` passes this literal to `-[StreamNetworkManager initHTTPServerProtocolWithServerAddress:]` | Observed. The only live game endpoint. |
| Server list | `dev.botme.com/lpserver1`, `lp110.m-botme.com/lp`, `203.194.225.201/lp`, `203.194.225.202/lp` | `+[NetworkHandler getServerAddressWithIndex:]` @ `0x1a3068` | Observed dead code: no caller. |
| Push-notification registration | `http://lp110.m-botme.com/apnlp/incomingClientRequest` | `-[PushNotificationHandler sendClientRequestEvent:]` @ `0x140bc4` | Observed. Sent only when an APNs device token exists. Stub. |
| Leaderboard (Facebook high scores) | `http://192.168.1.20/alifegamehighscore/leaderboard` (development flag set) or `http://www.botme.com/gameslist/leaderboard` | `-[HighScoreManager initWithReceiverDelegate:andWithEncryption:forDevelopment:]` @ `0x2aa524`; `-[Facebook init…]` passes `forDevelopment:YES` | Observed. Points at a private LAN address in the shipped build. Stub. |
| Facebook REST, Twitter OAuth | `api.facebook.com`, `api.twitter.com` | `FBRequest`, `GCTwitterAuth` | Observed. Third-party; out of scope. |
| Web links (terms of use, ads) | `www.botme.com/mobile/lp/*/terms_of_use.html`, `www.botme.com/ref/19xx`, `m.botme.com/mobile`, `click.linksynergy.com/…` | `LoginComponent`, `OptionComponent`, `MediaPlayerComponent`, `SceneUIComponent` | Observed. Opened in a web view or Safari; not protocol. Local copies `terms_of_use.html` and `lp_terms_of_use_offline.html` ship in the bundle. |

There is no HTTP-fetched configuration and no redirect-based server selection. The client follows an HTTP `302` `Location` header on the game channel (`-[HTTPServerChannel httpClient:redirectURL:withRequest:]` @ `0x2b4cd8`), which a server can use but does not need. An alternative transport, `HPServerChannel` (separate request and response addresses), exists in the binary but is never constructed (`initHPServerProtocolWithRequestAddress:responseAddress:` has no caller).

## HTTP layer

The game implements its own HTTP/1.1 client (`HTTPClient`, user agent `AlifeNetworking/1.0 HTTPClient <hw.machine>`) on top of `AsyncSocket`, which opens the TCP connection with `CFStreamCreatePairWithSocketToHost`. `NSURLConnection` is not used for the game channel.

### Requests the client sends

All requests go to the path of the servlet URL (`/lp/httpGDServlet`), with `Host: lp110.m-botme.com`. Observed in `-[HTTPClient sendRequest:]` @ `0x2b8bcc`, `-[HTTPServerChannel setupServerChannel]` @ `0x2b5304`, `-[HTTPServerChannel sendPollingRequest]` @ `0x2b6210`, `-[HTTPServerChannel write:withTag:]` @ `0x2b5ee0`.

| Request | Method | Headers | Body | When |
|---|---|---|---|---|
| Channel init | `GET` | `Action: INIT_CHANNEL` (and `Cookie: JSESSIONID=<id>` if one is known) | none | Once when the channel is created. If the TCP connect fails, the client logs `client init fail` and the channel is discarded. |
| Poll | `GET` | `Cookie: JSESSIONID=<id>` (if known), `Pooling: TRUE` (sic) | none | From a background thread every 12 s; every 1.5 s while "quick response mode" is active (started during login and IM chat). The first poll is sent immediately. |
| Send | `POST` | `Cookie: JSESSIONID=<id>`, `Connection: Close`, `Content-Length` | exactly one PDU (see below) | For each outgoing batch. |

Header names and the header block are `Name: value\r\n` lines; header order is unspecified (dictionary order). `autoCloseConnection` is set, so the client closes the TCP connection after each response; expect one TCP connection per request.

The poll and the send share one `HTTPClient`. While a request is in flight the client cannot send another; a blocked send is retried on the next 0.1 s tick (`+[NetworkHandler _sendBufferedMessage]` @ `0x1a3d90`). **The server must answer polls immediately (no long polling)**, or outgoing messages stall until the poll completes. Inferred from the shared client; not tested.

### Responses the client expects

Parsed in `-[HTTPClient onSocket:didReadData:withTag:]` @ `0x2b81bc` and `-[HTTPServerChannel httpClient:didReceivedResponse:]` @ `0x2b64f0`.

- Status line: `HTTP/1.x <code> <reason>`. The line must contain at least three space-separated parts. `200` is processed; `302` is followed using `Location`; anything else is only logged and the request is dropped.
- Header lines are split on the exact separator `": "`. Header names are matched case-insensitively.
- Body: send `Content-Length`. Chunked encoding is handled only for a single small chunk; do not use it.
- Session headers:

| Response to | Required headers | Body |
|---|---|---|
| Channel init | `Set-Cookie: JSESSIONID=<id>` (value must start with exactly `JSESSIONID=`; the client takes the text up to the first `"; "`). **No** `SESSION_VALID` header. | Ignored. Use `Content-Length: 0`. |
| Poll and send | `SESSION_VALID: TRUE` | Message container (below). An empty body or a count of 0 means "nothing queued". |
| Poll or send with an unknown or expired session | `SESSION_VALID: FALSE` | Ignored. The client logs `session invalid, please connect again`, disconnects the channel and reports the network as down. |

If a `SESSION_VALID: TRUE` response also carries a header named `HTTP-Response-Status`, its body is ignored (observed; purpose unknown).

A response without `SESSION_VALID` is always treated as a channel-init answer, whichever request it belongs to. On the channel-init answer the client marks the channel connected and writable (`ServerChannelManager sendChannelWrittable:`).

### Response body container

A `SESSION_VALID: TRUE` body is a list of PDUs:

```
uint32 count                       // big-endian
repeat count times:
    uint32 length                  // big-endian
    byte[length] pdu               // one complete PDU, see next section
```

The client appends each item to its input buffer and immediately parses one PDU from it (`StreamNetworkManager channelReadAvailable:` @ `0x2c49c0`). Put exactly one PDU in each item.

The `POST` body is a single PDU with no count prefix (observed: `outputBuffer` is never filled, so the body is only the new PDU).

## PDU and payload framing

### PDU

Built in `+[StreamMessageFactory getDataFromPDU:]` @ `0x2c3264` and `-[StreamNetworkManager internalSend:]` @ `0x2c41fc`; read in `+[StreamMessageFactory readDataFromChannel:]` @ `0x2c3174`.

| Offset | Size | Value |
|---|---|---|
| 0 | 1 | `0x02` (the client sends 2; it does not check the value it receives) |
| 1 | 1 | `0x00` (not checked) |
| 2 | 4 | `length` of the payload, big-endian |
| 6 | `length` | obfuscated session payload |

### Obfuscation

The payload is XORed byte by byte with the ASCII key `LNKINPARK_ENCRYPTOR_KEY_ALIFE` (29 bytes), restarting at key index 0 for every PDU: `out[i] = in[i] ^ key[i % 29]`. The same operation decodes. Observed: `+[GCEncryptor XORUsingValue:andKey:]` @ `0x2c9154`; the key and `setDoEncryption:YES` are set in `+[NetworkHandler startStreamNetworkManager]`; applied in both directions in `internalSend:` and `channelReadAvailable:`. The PDU header is not obfuscated.

There is no compression, checksum, sequence number or MAC.

### Session payload (after XOR)

Built by `-[StreamSessionDataParser convertMiddlewareSessionPayloadToBinary:]` @ `0x2b9c8c`, parsed by `convertBinaryToMiddlewareSessionPayload:` @ `0x2b9f38`.

| Field | Encoding |
|---|---|
| message type name | `uint32` length + ASCII bytes, for example `LOGIN` |
| session ID | `uint32` length + ASCII bytes. The client sends `""` until the handshake assigns one, then echoes it on every PDU. |
| body | the rest of the payload: message fields as listed in the catalogue |

### Combined messages

The client always batches outgoing game messages (`isCombinedMessage` is set to 1 in `+[NetworkHandler initNetworkHandler]` @ `0x1a3858`). A batch is one PDU whose type name is `STREAM_COMBINED_MESSAGE` and whose body is:

```
uint32 count
repeat count times:
    uint32 length
    byte[length] item   // uint32 nameLength + name + message body   (no session field)
```

Observed: `-[StreamDataParser convertStreamCombinedMessageToNSData:]` @ `0x2ba96c`, `convertNSDataToStreamCombinedMessageData:` @ `0x2ba804`, `-[StreamNetworkManager convertMessagesToCombinedMessage:]` @ `0x2c355c`, `convertCombinedMessageToMessages:` @ `0x2c33f0`.

The server can send either single messages (one PDU per message, type name = the message) or `STREAM_COMBINED_MESSAGE` PDUs; the client accepts both.

### Message body

Every game message starts with the `LPBaseMessage` header, then its own fields:

| Field | Type | Notes |
|---|---|---|
| `playerID` | `int32` | Client → server: the client's current player ID, copied into every message when it is constructed (`-[LPBaseMessage initWithMessageType:]` @ `0xb9498` reads `-[AccountConnector getPlayerID]`). That ID is 0 before login, becomes the **header** `playerID` of a successful `LOGIN_RESPONSE`, and is replaced by `playerProfile.playerID` of a successful `CREATE_PROFILE_RESPONSE`. Server → client: read only for `LOGIN_RESPONSE`; ignored otherwise. Codec: `-[LPServerMessageParser initLPBaseMessage:withNSData:]` @ `0x198a6c`, `convertLPBaseMessageToNSData:` @ `0x198ae8`. |

## Primitive encoding

All multi-byte integers are big-endian. Observed in `Utils` (`0x2c4e10`–`0x2c576c`) and in every `LPServerMessageParser` encoder and decoder.

| Type | Wire form | Notes |
|---|---|---|
| `int32` | 4 bytes, signed, big-endian | Also used for enums, IDs, counts and amounts. |
| `bool` | 1 byte: `0` false, anything else true | |
| `string` | `uint32` byte length + bytes, no terminator | The client encodes UTF-8 (`lengthOfBytesUsingEncoding:NSUTF8StringEncoding`). It decodes with `+[NSString stringWithCString:length:]`, which uses the C-string default encoding (MacRoman on iOS). Send ASCII to be safe. A nil string is sent as length 0. |
| `datetime` | same as `string`; text `YYYYMMDDhhmmss` (`%Y%m%d%H%M%S`, 14 characters) | Parsed with `sscanf("%04d%02d%02d%02d%02d%02d")` where the client needs a date. Timezone is not specified (inferred: server local time). |
| `bytes` | `uint32` length + raw bytes | Used for apartment layouts, mail bodies, sound clips, device token. |
| `array` of X | `uint32` count + count × X | X is `int32`, `string` or an entity. |
| entity | its fields concatenated in order, no length prefix and no type tag | There is no nil entity: the decoder always reads every field, so the server must always send complete structures. |

There are no `int16`, `int64` or floating-point fields in any message.

Truncated messages make the client read past the end of the buffer (no bounds checks in the decoders). Always send every field.

## Connection handshake and session

Observed in `StreamNetworkManager channelReadAvailable:` @ `0x2c49c0`, `sendConfirmation` @ `0x2c38d4`, and `HTTPServerChannel httpClient:didReceivedResponse:` @ `0x2b64f0`.

1. Client: `GET` with `Action: INIT_CHANNEL`.
2. Server: `200`, `Set-Cookie: JSESSIONID=<cookie>`, no `SESSION_VALID`, empty body. The channel is now connected; polling starts (first poll immediately).
3. Client: `GET` poll with the cookie.
4. Server: `SESSION_VALID: TRUE`, body with one PDU of type `CHANNEL_SETUP_CONFIRMATION` and `channelStatus = 0`.
5. Client: `POST` with a `CHANNEL_SETUP_CONFIRMATION`, `channelStatus = 1`, session ID `""`.
6. Server: replies (in that `POST` response or the next poll) with `CHANNEL_SETUP_CONFIRMATION`, `channelStatus` ≠ 0 (use 1), and the **session ID field of the session payload** set to a non-empty string.
7. Client stores that string as its session ID, logs `handshake finished`, and starts sending queued game messages.

`CHANNEL_SETUP_CONFIRMATION` body: one `int32 channelStatus`. Only zero versus non-zero matters. Steps 4 and 5 are optional: a server can send step 6 as the first poll answer and the client accepts it (observed: any non-zero status with a non-empty session ID completes the handshake).

Until step 7 the client does not send game messages; they stay queued. Every later PDU from the client carries the session ID in its session field and the cookie in its `Cookie` header. The server can key client state on either.

There is no application-level keep-alive or heartbeat message; the 12-second poll is the keep-alive. The client has no request IDs: replies are matched to requests by message type.

### Disconnect and reconnect

- An HTTP timeout, a socket error or `SESSION_VALID: FALSE` disconnects the channel and calls `networkManagerDidDisconnected` on the listeners. `AccountConnector` then logs the player out of online features (`-[AccountConnector networkManagerDidDisconnected]` @ `0x4e27c`).
- `AccountHandler` keeps a network mode (observed `isDisconnectedMode` … `isOnlineReconnectMode` @ `0x20da0c`–`0x20da88`): `0` offline, `1` guest, `2` guest-reconnect, `3` online, `4` online-reconnect. In mode 2 the client restarts the channel and sends `GUEST_LOGIN` every 20 s; in mode 4 it re-sends the stored `LOGIN` every 20 s (`-[AccountHandler cycle]` @ `0x2106e8`).
- While offline, the client queues some messages (for example `ADD_ITEM`, `PLAYER_PROFILE_UPDATE`) in a buffer file and replays them after a successful login (`-[AccountHandler sendBufferMessageToNetworkHandler]` @ `0x20f210`, `loadBufferMessagesFromFile` @ `0x20eeb4`). The replayed messages get the current profile's `playerID`.

## Client send loop and batching

`+[NetworkHandler sendMessage:]` only appends to a queue. A background thread wakes every 0.1 s (`_runloop` @ `0x1a30d4`) and calls `_sendBufferedMessage`, which wraps everything queued into one `STREAM_COMBINED_MESSAGE` and `POST`s it. If the `POST` cannot start, the batch is kept and retried. Incoming messages are queued too and handed to listeners one per frame on the main thread (`+[NetworkHandler dispatchNetworkEvent]` @ `0x1a3a50`), each listener receiving `receiveNetworkMessage:`.

Consequences for a server:

- Several requests can arrive in one `POST`. Process items in order.
- Replies can be queued and returned in the `POST` response or in the next poll.
- Ordering between different message types is preserved by the client queue.

## Catalogue schema (`messages.json`)

`server/protocol/messages.json` is generated from the extraction and is enough to encode and decode every message generically.

```jsonc
{
  "schema_version": 1,
  "primitives": { "int32": "...", "bool": "...", "string": "...", "datetime": "...", "bytes": "...", "array": "...", "entity:<Name>": "..." },
  "transport_messages": {
    "_xor_key": "LNKINPARK_ENCRYPTOR_KEY_ALIFE",
    "CHANNEL_SETUP_CONFIRMATION": { "id", "direction", "fields", "notes", "evidence" },
    "STREAM_COMBINED_MESSAGE":    { ... }
  },
  "entities": {
    "<EntityName>": {
      "fields": [ { "name": "playerID", "type": "int32" }, ... ],
      "evidence": { "encoder": "0x...", "decoder": "0x..." },
      "notes": "optional", "used": false          // "used" present only for entities no message carries
    }
  },
  "messages": {
    "<TYPE_NAME>": {
      "id": "<TYPE_NAME>",             // the string sent on the wire as the message type name
      "class": "ObjC class name",
      "direction": "c2s | s2c | both", // for status "wire-only" the direction is inferred from the name
      "pairs_with": [ "<TYPE_NAME>" ], // replies the client expects (or the server should produce)
      "status": "full | partial | wire-only",
      "fields": [ { "name": "...", "type": "<primitive or entity:Name or array>", "of": "<element type, arrays only>", "notes": "optional" } ],
      "client_handling": "what the client does",
      "minimal_server_reply": "what a server must send back",
      "evidence": { "encoder": "0x...", "decoder": "0x...", "code": "handler functions" }
    }
  }
}
```

Rules for a generic codec:

- A message body is its `fields` in order. The first field is always `playerID` (the `LPBaseMessage` header); the two transport messages have no header.
- `type` is one of `int32`, `bool`, `string`, `datetime`, `bytes`, `array`, or `entity:<Name>`. For `array`, `of` gives the element type (`int32`, `string` or `entity:<Name>`).
- Entities are inlined with no prefix; resolve them from `entities`.
- `status` meanings: `full` = wire layout and client behaviour read; `partial` = wire layout read, behaviour partly known; `wire-only` = wire layout read, but this client never sends or handles the message.

Field names are the ObjC property names, including the original misspellings (`targetplayerID` in `SEND_MAIL_RESPONSE`, type names `MISSION_STATE_RESPOSNE` and `DEVICE_TOKEN_UPADTE_RESPONSE`).

## Flows

Every flow below starts after the [connection handshake](#connection-handshake-and-session). "Reply" means queue the message for the client's next poll or `POST` response.

### Startup and first connect

1. The game starts offline. Nothing connects until the player creates a profile or logs in.
2. New player: the character editor creates a local offline profile, then sets network mode 2 (guest-reconnect) (`CreateProfileComponent` @ `0x49244`). From then on, every 20 s, the client restarts the channel and sends `GUEST_LOGIN{version=1}` until it gets a `LOGIN_RESPONSE` (`-[AccountHandler cycle]` @ `0x2106e8`). This is the `Call connect to server` / `client init fail` sequence seen in touchHLE.
3. Returning registered player: network mode 4 (online-reconnect); every 20 s the client sends the stored `LOGIN` (`loginPreviousAccount` @ `0x4d21c`).
4. If no server answers, the game keeps working offline and keeps retrying.

### Guest login

| Step | Client | Server |
|---|---|---|
| 1 | `GUEST_LOGIN{playerID=0, version=1}` | Create or look up a guest account. |
| 2 | | `LOGIN_RESPONSE{playerID=<new player ID>, responseCode=5, account, profile}` |
| 3 | Mode becomes 1 (guest). Account stored, header player ID set, startup process runs (below). The local profile is kept. | |

`responseCode 0` or `4` instead moves the client to mode 3 (online) and, for 0, replaces the local profile with `profile`. For a guest, 5 is the safe answer (inferred from `AccountConnector receiveNetworkMessage:` @ `0x4e30c`: 5 → `setNetworkMode:1`, 0/4 → `setNetworkMode:3`).

Guest mode sends presence, profile views, ranks, bulletin and apartment messages; friends, mail, ratings, items, achievements and the store go through local-only paths. Guests cannot send mail to others (mail is written to local SQLite).

### Account creation and login

1. `CREATE_ACCOUNT{account{userName, password, email, optin, UDID, osVersion, deviceType, deviceToken…}}`.
2. Reply `CREATE_ACCOUNT_RESPONSE{responseCode}`: 0 created, 1 name taken, 2 email taken.
3. On 0 the client sends `LOGIN{userName, password (plaintext), version=1}`.
4. Reply `LOGIN_RESPONSE{playerID, responseCode, account, profile}`:
   - 0: account has a profile. `profile.profileName` **must equal `userName` (case-insensitive)**, or the client treats the answer as missing and its spinner never stops.
   - 4: account exists but has no profile; the client goes to profile creation.
   - 1: wrong name or password ("Invalid Name or Password").
5. After 0 the client sends `GAME_ID_UPDATE{accountID, gameID=4}`, `ACCOUNT_UPDATE` (UDID and device token) and another `GAME_ID_UPDATE`. Reply `ACCOUNT_UPDATE_RESPONSE{responseCode=0, account}`.
6. The client replays its offline buffer (`ADD_ITEM`, `DELETE_ITEM`, `ACHIEVEMENT_GAIN`, `UPDATE_APARTMENT_DATA`, `PLAYER_PROFILE_UPDATE`), rewriting each `playerID`. Expect a burst.

The user name is the profile name: the client compares them on login. Name validation is client-side (alphanumeric and underscore, profanity list, `BlockedNames` table).

### Profile creation

1. `CREATE_PROFILE{profile}` with `status=3`, `receiveNotification=1`.
2. Reply `CREATE_PROFILE_RESPONSE{responseCode=0, playerProfile}` where `playerProfile.playerID` is the new real ID. The client adopts it as its player ID for every later header.
3. The client sends `GAME_ID_UPDATE`, and `ADD_LOG` only if the app was re-signed with `SignerIdentity` in `Info.plist`.

When an offline or guest player converts to an online account, the startup process re-sends every non-default item (`ADD_ITEM`), every achievement (`ACHIEVEMENT_GAIN`) and the apartment (`UPDATE_APARTMENT_DATA`), then `GOTO_APARTMENT`.

### Startup process (after login)

`-[AccountHandler startUpProcess]` @ `0x2111b8` sends, in this order, with the replies the server should queue:

| Client sends | Reply |
|---|---|
| `GET_ALL_PLAYER_ITEMS` | `ITEM_LIST` (mandatory; empty allowed) |
| `GET_ALL_REQUEST` | `REQUEST_LIST` (empty allowed) |
| `GET_PLAYER_ALL_ACHIEVEMENT{targetPlayerID=self}` | `PLAYER_ACHIEVEMENT_LIST{targetPlayerID=self, achievementList}`; without it no achievements are awarded this session |
| `GET_ALL_NEWS` | `NEWS_LIST` (empty allowed) |
| `GET_ALL_FRIENDS_PROFILE` (first startup, online) | `FRIEND_PROFILE_LIST` |
| `GET_BLACKLIST_PROFILES` | `BLACKLIST_PROFILE_LIST` |
| `GET_ALL_MAIL` | `MAIL_LIST` |
| `GET_PROFANITY_WORDS_ENTITY` | `PROFANITY_WORD_ENTITIES` (seed from `lp.sqlite`) |

### Change location and presence

1. On every scene entry except the apartment, the client sends `CHANGE_LOCATION{locationID}` and clears the other avatars.
2. Reply `LOCATION_PLAYER_PROFILE{playerProfileList}` with the other online players whose last location is that ID. Exclude the requester: the client does not filter itself out.
3. The client spawns each profile once as a standing avatar at a random x position. There are no movement, join or leave messages: presence is a snapshot taken when the player enters a scene. Another `LOCATION_PLAYER_PROFILE` adds more avatars without removing old ones.

The client also tracks its location in `PLAYER_PROFILE_UPDATE.playerProfile.locationID`.

Location IDs (from the `addAvatarComponent:` callers in each panel):

| ID | Scene | ID | Scene | ID | Scene |
|---|---|---|---|---|---|
| 0 | Central Park | 12 | Casino Tattoo | 24 | Soho Coffee |
| 1 | Beach | 13 | Beach Coffee | 25 | Soho Pixxel Bar |
| 2 | Park | 14 | Beach Bourdon | 26 | Soho Pool Hall |
| 3 | Soho | 15 | Art Gallery | 27 | LP Underground |
| 4 | Casino | 16 | West Side Coffee | 28 | Alife Shop |
| 5 | West Side | 17 | Park Coffee | 29 | Hospital |
| 6 | Downtown | 18 | Delson Park | 30 | Beach Hot Dog |
| 7 | Apartment (no `CHANGE_LOCATION`) | 19 | Park Pixel Gift | 31 | Casino Construction |
| 8 | Central Park Coffee Shop | 20 | Downtown Joyroom | 32 | Mall |
| 9 | Boss game | 21 | Downtown Suru | 33 | Pre-boss 2 |
| 10 | Casino Row | 22 | Downtown Pixxel Bar | 34 | Boss 2 |
| 11 | Casino Bar | 23 | Soho Phoenix Pub | | |

The client ignores presence in 7, 9, 29, 31, 33 and 34, and in some mission states.

### Apartments

- Entering an apartment sends `GOTO_APARTMENT{apartmentID}` (the first number of `profile.apartmentIDList`). For another player's apartment, or one's own when no local copy exists, the client waits on a spinner for `APARTMENT_INFO{apartment{apartmentID, apartmentName, apartmentData, districtID}, playerProfileList=[]}`. Send the blob last stored by `UPDATE_APARTMENT_DATA`, or a single `0x00` byte for an empty apartment (inferred from the client's own offline default).
- Saving in the apartment editor sends `UPDATE_APARTMENT_DATA{apartmentID, apartmentData}`. No reply.

### Items and the store

- Items gained: `ADD_ITEM{item{realPlayerItemID=0, tempPlayerItemID=n, itemID, itemType=itemID/1000, applePurchased=0}}`. Reply `ADD_ITEM_RESPONSE{item}` with `tempPlayerItemID` echoed unchanged and a new unique `realPlayerItemID`. If the reply is lost, the client re-sends the item as a new `ADD_ITEM` after the next `ITEM_LIST`, which duplicates it; make `ADD_ITEM` idempotent per (player, itemID) if duplicates matter.
- Items removed: `DELETE_ITEM{playerItemID}`. No reply.
- Purchases use in-game credits only. There is no StoreKit code and every `apple_product_identifier` in `lp.sqlite` is `'0'`. The store reads its catalogue from local `lp.sqlite` (`product`, `productitem`), never from the server. On purchase the client sends `BUY_PRODUCT{productID, itemList}` (price not included). Reply `BUY_PRODUCT_RESPONSE{responseCode=0, itemList}` with the product's items, each with a unique `realPlayerItemID` and `tempPlayerItemID=0`. The client then deducts the price itself and sends `PLAYER_PROFILE_UPDATE` with the new `credits`. `responseCode` is not read, so the server cannot refuse a purchase.

### Achievements, missions, fights

- `ACHIEVEMENT_GAIN{achievement{tempPlayerAchievementID, achievementID 22000–22014, achievementValue, timestamp}}` → reply `ACHIEVEMENT_GAIN_RESPONSE{responseCode=0, achievement}` with the temp ID echoed and `playerAchievementID` assigned.
- Missions and fight results are stored only on the device (`PlayerMission`, `FightResult` tables). The mission and fight messages are never sent.

### Friends, requests and IM

The client never handles an incoming `RETURN_RESPONSE`, so the server translates answers into pushes:

1. **A sends a friend request to B** (`SEND_REQUEST`, `request.type=1`): store it with a `requestID`; if B is online push `REQUEST_LIST{rList=[request]}` to B with `fromPlayerProfile` = A's profile; include it in B's `GET_ALL_REQUEST` reply until answered.
2. **B answers** (`RETURN_RESPONSE{requestID, requestType=1, responseCode, targetPlayerID=A}`): delete the request. On 0, create the friendship and push `NEW_FRIEND_MADE{friendProfile=B}` to A and `NEW_FRIEND_MADE{friendProfile=A}` to B (inferred: B's client does not add the friend on accept).
3. **A asks B to chat** (`SEND_REQUEST`, `type=0`, `isInstance=1`): if B is online push `REQUEST_LIST` with A's profile; otherwise push `IM_BUSY{targetPlayerID=B}` to A (inferred). B's client shows an accept popup, or auto-answers 1 (A is blocked) or 2 (busy, two rooms open, or B's `status` is 0).
4. **B answers the chat request**: 0 → push `IM_START{targetPlayerProfile=B}` to A and `IM_START{targetPlayerProfile=A}` to B; 1 → `IM_REJECT{targetPlayerID=B}` to A (optional); 2 → `IM_BUSY{targetPlayerID=B}` to A.
5. **`IM_MESSAGE` from A to B**: forward to B with `targetPlayerID=A` and the `message` bytes unchanged.
6. **`IM_STOP` from A to B**: forward to B with `targetPlayerID=A`. Recommended: send `IM_STOP` to chat partners when a player disconnects (inferred).
7. **Poke** (`type=3`): forward to the target as `REQUEST_LIST`; do not store.
8. **`REMOVE_FRIEND` from A for B**: delete the friendship; push `FRIEND_REMOVED{targetPlayerID=A}` to B (inferred).
9. **Online status**: the friend screen polls `GET_ONLINE_FRIENDS_STATUS` every 20 s; reply `ONLINE_FRIENDS_STATUS_LIST{playerStatusList=[{playerID, status}]}` for online friends, using each friend's own profile `status` (0 or 3).

`PlayerProfile.status` 3 means available and 0 means busy (inferred from the IM auto-decline and the profile toggle). New profiles are created with 3; keep it, or IM never works.

### Mail and bulletin

- `SEND_MAIL{targetPlayerID, chainID=0, message, subject, soundTrack}`: store it, reply `SEND_MAIL_RESPONSE{responseCode=0, targetplayerID, targetPlayerName}`, and push `NEW_MAIL{mail}` to the recipient if online (fill `mailID`, `senderPlayerID`, `senderName`, `created`, `updated`, `hadRead=false`). Missions send story mail to the player's own ID; store and deliver it like any other mail (inferred: online mode keeps no local copy).
- `CHECKED_MAIL{mailID}` marks read; `DELETE_MAIL{mailID}` deletes. No replies.
- `GET_ALL_BULLETIN_MESSAGE{locationID}` is polled every 30 s per open board; reply `ALL_BULLETIN_MESSAGE_LIST{postRemainTime, messageList}` newest first, all with the requested `locationID`. `ADD_BULLETIN_MESSAGE` stores a post; no reply. `NEW_BULLETIN_MESSAGE` pushes are optional.

### Ranks, ratings, profile views

- `GET_PLAYER_PROFILE{targetPlayerID}` → `PLAYER_PROFILE{playerProfile}` (mandatory; otherwise the spinner never stops).
- `CREW_RANK` → `CREW_RANK_RESPONSE{responseCode, rankList}`; `PLAYER_RANK{targetPlayerID, 0, 19}` → `PLAYER_RANK_RESPONSE{responseCode, playerRank, rankList}`. `RankEntity.rankID` is a player ID; `rankIndex` is 0-based; `reputation` is the displayed value.
- `GET_PLAYER_RATING_LIST{targetPlayerID, [1..5]}` → `PLAYER_RATING_LIST{playerRatingCountList=[{rating, numOfRating}]}`. Replies carry no target, so answer in request order.
- `ADD_PLAYER_RATING{targetPlayerID, rating}` → `ADD_PLAYER_RATING_RESPONSE{responseCode}`: 0 ok, 1 already rated.

### Media, Blackbirds and news

- `GET_BLACK_BIRDS{email}` is sent once in mission 6 ("Enter your email to receive the Blackbirds mp3"). The client never reads a reply and finishes the mission regardless.
- The media and soundtrack messages (`REQUEST_MEDIA`, `REQUEST_ALL_MEDIA`, `GET_SOUND_TRACK`) are never sent; music and videos ship in the bundle.
- `NEWS_LIST{newsList=[{newsID, created, subject, content, link}]}` fills the media-player news tab.

### Logout and errors

- The client has no logout message: its `LOGOUT` object is created and released without sending. Treat a session as gone when polls stop (the client polls every 12 s; a timeout of 60 s is a reasonable choice).
- The server can push `LOGOUT` (kick) or `GENERAL_ERROR_MESSAGE{errorCode, errorMessage}`; both log the player out of online features. `errorCode` 3 shows "Server Full", 4 "Server Maintenance".
- `SESSION_VALID: FALSE` on any poll disconnects the client; it then retries login every 20 s.
- **Never send `SEND_NOTIFICATION`**: its handler calls selectors the message class does not implement and throws.

## Message catalogue

Generated from `messages.json`. Field 0 of every game message is the `playerID` header. Codec addresses are the `LPServerMessageParser` encoder (`convert<Class>ToNSData:`) and decoder (`convertNSDataTo<Class>Data:` or `convertNSData:to<Entity>:`).

### Used by the client

#### `ACCOUNT_UPDATE`

Class `AccountUpdate` · client → server · pairs with `ACCOUNT_UPDATE_RESPONSE` · codec `0x00189ac4`/`0x00189964` · behaviour: AccountConnector 0x4dc64, 0x4d6e4, 0x4da8c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `account` | LPAccount |  |

**Client:** Sent after login (UDID/device token update), on email or password change. Always followed by GAME_ID_UPDATE.

**Server reply:** ACCOUNT_UPDATE_RESPONSE responseCode 0 with the stored account

#### `ACCOUNT_UPDATE_RESPONSE`

Class `AccountUpdateResponse` · server → client · codec `0x00197b38`/`0x001979a0` · behaviour: AccountConnector 0x4e30c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |
| 2 | `account` | LPAccount |  |

**Client:** responseCode 0 copies `account` into the local account; listeners get receiveAccountUpdateResponse: (password-change UI).

#### `ACHIEVEMENT_GAIN`

Class `AchievementGain` · client → server · pairs with `ACHIEVEMENT_GAIN_RESPONSE` · codec `0x00188f28`/`0x00188dc8` · behaviour: AchievementConnector 0x4fbb0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `achievement` | Achievement |  |

**Client:** Achievement 22000-22014 earned; tempPlayerAchievementID counter starts at 0 per launch; achievementValue '-1' or ''.

**Server reply:** ACHIEVEMENT_GAIN_RESPONSE

#### `ACHIEVEMENT_GAIN_RESPONSE`

Class `AchievementGainResponse` · server → client · codec `0x001968f0`/`0x00196758` · behaviour: AchievementConnector 0x50ec8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |
| 2 | `achievement` | Achievement |  |

**Client:** Adds achievement to the in-memory list, deduped by tempPlayerAchievementID; responseCode ignored.

#### `ADD_BULLETIN_MESSAGE`

Class `AddBulletinMessage` · client → server · codec `0x00185b80`/`0x00185a20` · behaviour: BulletinConnector 0x545cc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `message` | BulletinMessage |  |

**Client:** Post (client throttles to one per 15 s); no reply.

**Server reply:** none

#### `ADD_ITEM`

Class `AddItem` · client → server · pairs with `ADD_ITEM_RESPONSE` · codec `0x00186174`/`0x00186014` · behaviour: ItemConnector 0x58a98

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `item` | Item |  |

**Client:** Item gained (mission rewards, mail gifts, sync). item: realPlayerItemID 0, tempPlayerItemID = client counter starting at 1 per launch, itemType = itemID/1000.

**Server reply:** ADD_ITEM_RESPONSE echoing tempPlayerItemID with a new realPlayerItemID

#### `ADD_ITEM_RESPONSE`

Class `AddItemResponse` · server → client · codec `0x00185dd4`/`0x00185c74` · behaviour: ItemConnector 0x5ae5c, updateItemByTempID: 0x59d10

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `item` | Item |  |

**Client:** UPDATE PlayerItem SET player_item_id = real, temp = 0 WHERE temp = tempPlayerItemID. Refreshes the apartment for furniture types.

#### `ADD_LOG`

Class `AddLog` · client → server · codec `0x0018f070`/`0x0018ef08` · behaviour: PlayerConnector 0x5f304; GameCanvas checkCrack 0x6223c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `logType` | int32 |  |
| 2 | `logValue` | string |  |
| 3 | `districtID` | int32 |  |

**Client:** Only sent after CREATE_PROFILE_RESPONSE 0 when the IPA Info.plist contains SignerIdentity (piracy report): logType 0, logValue = profileName. Accept and ignore.

**Server reply:** none

#### `ADD_PLAYER_RATING`

Class `AddPlayerRating` · client → server · pairs with `ADD_PLAYER_RATING_RESPONSE` · codec `0x00193804`/`0x00193704` · behaviour: FriendConnector 0x54ef8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |
| 2 | `rating` | int32 |  |

**Client:** Star bar on a profile (online mode); rating 1-5.

**Server reply:** ADD_PLAYER_RATING_RESPONSE

#### `ADD_PLAYER_RATING_RESPONSE`

Class `AddPlayerRatingResponse` · server → client · codec `0x00193614`/`0x00193544` · behaviour: PlayerRatingComponent 0x145bbc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 | 0 ok, 1 duplicate |

**Client:** 0 'Thanks for rating!' then re-requests GET_PLAYER_RATING_LIST; 1 'Rated Already'; other 'Failed to Rate'.

#### `ADD_TOP_FRIEND`

Class `AddTopFriend` · client → server · codec `0x0019514c`/`0x0019507c` · behaviour: FriendConnector 0x56af4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `friendID` | int32 |  |

**Client:** Star toggle; no reply.

**Server reply:** none

#### `ALL_BULLETIN_MESSAGE_LIST`

Class `AllBulletinMessageList` · server → client · codec `0x0018cd20`/`0x0018caf0` · behaviour: BulletinComponent 0x15caac

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `postRemainTime` | int32 |  |
| 2 | `messageList` | array<BulletinMessage> |  |

**Client:** Newest first; ignored if messageList[0].locationID differs from the open board; merged by timestamp string.

#### `APARTMENT_INFO`

Class `ApartmentInfo` · both directions · codec `0x0018c4f4`/`0x0018c268` · behaviour: ApartmentComponent 0x37ed0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `apartment` | Apartment | apartmentData = last UPDATE_APARTMENT_DATA blob, or 1 byte 0x00 for empty |
| 2 | `playerProfileList` | array<PlayerProfile> |  |

**Client:** Stops spinner 0 and loads apartment.apartmentData. playerProfileList is unused. (The client also builds one locally offline, hence 'both'.)

#### `BLACKLIST_PROFILE_LIST`

Class `BlackListProfileList` · server → client · codec `0x00194b78`/`0x00194974` · behaviour: BlacklistConnector 0x529ec

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `profileList` | array<PlayerProfile> |  |

**Client:** Replaces the local Blacklist table.

#### `BLOCK_PLAYER`

Class `BlockPlayer` · client → server · codec `0x001946c4`/`0x001945f4` · behaviour: BlacklistConnector 0x52bc0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** Adds to local Blacklist then sends; no reply.

**Server reply:** none

#### `BUY_PRODUCT`

Class `BuyProduct` · client → server · pairs with `BUY_PRODUCT_RESPONSE` · codec `0x0018dcb0`/`0x0018da80` · behaviour: ProductConnector 0x6020c; StoreComponent 0x92764

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `productID` | int32 |  |
| 2 | `itemList` | array<Item> |  |

**Client:** Store purchase with in-game credits (no StoreKit). itemList = the product's items (only itemID/itemType set). Price is not sent; the client deducts credits itself and then sends PLAYER_PROFILE_UPDATE.

**Server reply:** BUY_PRODUCT_RESPONSE

#### `BUY_PRODUCT_RESPONSE`

Class `BuyProductResponse` · server → client · codec `0x0018d8c8`/`0x0018d698` · behaviour: ProductConnector 0x60d64; StoreComponent 0x92858

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |
| 2 | `itemList` | array<Item> | the product's items with unique realPlayerItemID, tempPlayerItemID 0 |

**Client:** responseCode is not read. Inserts each item locally, deducts credits, 'BUY_SUCCESS' popup, stops spinner 2.

#### `CHANGE_LOCATION`

Class `ChangeLocation` · client → server · pairs with `LOCATION_PLAYER_PROFILE` · codec `0x00192a00`/`0x00192930` · behaviour: PlayerConnector 0x5f0d8; LPBasePanel addAvatarComponent:

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `locationID` | int32 |  |

**Client:** On every scene entry except the apartment (location 7). Location IDs are listed in protocol.md.

**Server reply:** LOCATION_PLAYER_PROFILE (may be empty)

#### `CHECKED_MAIL`

Class `CheckedMail` · client → server · codec `0x0018f63c`/`0x0018f56c` · behaviour: MailConnector 0x5c690

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mailID` | int32 |  |

**Client:** Mail opened; no reply.

**Server reply:** none

#### `CREATE_ACCOUNT`

Class `CreateAccount` · client → server · pairs with `CREATE_ACCOUNT_RESPONSE` · codec `0x0018961c`/`0x001894bc` · behaviour: AccountConnector 0x4de58, 0x4df58

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `account` | LPAccount |  |

**Client:** Sent from the account form (createOnlineAccount:isOptin:) and when converting an offline/guest player (createAccount:). account carries userName, password, email, optin, UDID, device info.

**Server reply:** CREATE_ACCOUNT_RESPONSE

#### `CREATE_ACCOUNT_RESPONSE`

Class `CreateAccountResponse` · server → client · codec `0x0019897c`/`0x001988ac` · behaviour: AccountConnector 0x4e30c; LoginComponent cycle 0x29cf0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 | 0 ok, 1 name taken, 2 email taken |

**Client:** responseCode 0 = created: the client moves its local database to the new user name and sends LOGIN; 1 = 'This name has already been taken by another player'; 2 = 'This email has already been taken by another player'; other = back to form.

#### `CREATE_PROFILE`

Class `CreateProfile` · client → server · pairs with `CREATE_PROFILE_RESPONSE` · codec `0x00189870`/`0x00189710` · behaviour: PlayerConnector createProfile: 0x5f9bc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `profile` | PlayerProfile |  |

**Client:** Character creation (CreateProfileComponent confirm, CoreUIPanel). Full profile, status=3, receiveNotification=1. Not sent in guest-reconnect/offline mode (local fake response 0).

**Server reply:** CREATE_PROFILE_RESPONSE

#### `CREATE_PROFILE_RESPONSE`

Class `CreateProfileResponse` · server → client · codec `0x0019786c`/`0x001976d4` · behaviour: PlayerConnector 0x5f304; CreateProfileComponent 0x48aec; CoreUIPanel 0x10fbcc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |
| 2 | `playerProfile` | PlayerProfile | must carry the new real playerID |

**Client:** responseCode 0: playerProfile becomes the self profile and its playerID becomes the client's player ID; client sends GAME_ID_UPDATE (and ADD_LOG if the IPA is cracked). Other codes leave the create screen waiting (no error popup found).

#### `CREW_RANK`

Class `CrewRank` · client → server · pairs with `CREW_RANK_RESPONSE` · codec `0x00186a44`/`0x001869a8` · behaviour: PlayerConnector 0x5e40c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Sent with PLAYER_RANK.

**Server reply:** CREW_RANK_RESPONSE

#### `CREW_RANK_RESPONSE`

Class `CrewRankResponse` · server → client · codec `0x001901e0`/`0x0018ffb0` · behaviour: BaseProfileComponent 0x24cdfc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |
| 2 | `rankList` | array<RankEntity> |  |

**Client:** Fills the crew rank table (spinner 5). responseCode ignored.

#### `DELETE_ITEM`

Class `DeleteItem` · client → server · codec `0x0018e2c0`/`0x0018e1f0` · behaviour: ItemConnector 0x59788

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerItemID` | int32 |  |

**Client:** Item discarded/consumed/gifted; playerItemID = local player_item_id. No reply; ignore unknown IDs.

**Server reply:** none

#### `DELETE_MAIL`

Class `DeleteMail` · client → server · codec `0x0018f7fc`/`0x0018f72c` · behaviour: MailConnector 0x5c8e0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mailID` | int32 |  |

**Client:** No reply.

**Server reply:** none

#### `FORGOT_PASSWORD`

Class `ForgotPassword` · client → server · pairs with `FORGOT_PASSWORD_RESPONSE` · codec `0x00189270`/`0x00189168` · behaviour: AccountConnector 0x4d0ac

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `emailAddress` | string |  |

**Client:** Forgot-password form.

**Server reply:** FORGOT_PASSWORD_RESPONSE

#### `FORGOT_PASSWORD_RESPONSE`

Class `ForgotPasswordResponse` · server → client · codec `0x001975e4`/`0x00197514` · behaviour: AccountConnector 0x4e30c; LoginComponent 0x28e80

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 | 0 sent, 1 unknown email |

**Client:** responseCode 1 -> forgotPasswordSuccess:NO ('this email address has not been registered'), 0 -> forgotPasswordSuccess:YES ('An email has been sent').

#### `FRIEND_PROFILE_LIST`

Class `FriendProfileList` · server → client · codec `0x001965d4`/`0x001963d0` · behaviour: FriendConnector 0x55994; FriendComponent 0x202cf0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `friendProfileList` | array<FriendProfile> |  |

**Client:** Replaces the local Friends table. friendType 0 normal, 1 top friend. friendProfile.status 0 or 3 seeds the online list.

#### `FRIEND_REMOVED`

Class `FriendRemoved` · server → client · codec `0x0019530c`/`0x0019523c` · behaviour: FriendConnector 0x55994; FriendComponent 0x202688

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** Push: removes targetPlayerID (the remover) from the friend tables.

#### `GAME_ID_UPDATE`

Class `GameIDUpdate` · client → server · codec `0x00198788`/`0x00198688` · behaviour: AccountConnector 0x4e30c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `accountID` | int32 |  |
| 2 | `gameID` | int32 | always 4 |

**Client:** Fire-and-forget after login success, profile creation and every ACCOUNT_UPDATE. gameID is always 4.

**Server reply:** none

#### `GENERAL_ERROR_MESSAGE`

Class `GeneralErrorMessage` · server → client · codec `0x00189e78`/`0x00189d40` · behaviour: AccountConnector 0x4e30c; LPBasePanel 0x113bb0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `errorCode` | int32 | 3 server full, 4 maintenance |
| 2 | `errorMessage` | string |  |

**Client:** Logs errorMessage, stops startup, logs out of online features. errorCode 3 shows 'Server Full', 4 shows 'Server Maintenance' (LPBasePanel).

#### `GET_ALL_BULLETIN_MESSAGE`

Class `GetAllBulletinMessages` · client → server · pairs with `ALL_BULLETIN_MESSAGE_LIST` · codec `0x0018cfa8`/`0x0018ced8` · behaviour: BulletinConnector 0x543c8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `locationID` | int32 |  |

**Client:** Every 30 s while a bulletin board is open and 5 s after posting.

**Server reply:** ALL_BULLETIN_MESSAGE_LIST (empty ok)

#### `GET_ALL_FRIENDS_PROFILE`

Class `GetAllFriendsProfile` · client → server · pairs with `FRIEND_PROFILE_LIST` · codec `0x00187e6c`/`0x00187dd0` · behaviour: FriendConnector 0x551f8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** At startup (online) and from the friend screen.

**Server reply:** FRIEND_PROFILE_LIST (empty ok)

#### `GET_ALL_MAIL`

Class `GetAllMail` · client → server · pairs with `MAIL_LIST` · codec `0x001866a4`/`0x00186608` · behaviour: MailConnector 0x5d584

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** At startup and on refresh.

**Server reply:** MAIL_LIST (empty ok)

#### `GET_ALL_NEWS`

Class `GetAllNews` · client → server · pairs with `NEWS_LIST` · codec `0x001851ec`/`0x00185150` · behaviour: NewsConnector 0x1a4940

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** End of every startup process.

**Server reply:** NEWS_LIST (empty ok)

#### `GET_ALL_PLAYER_ITEMS`

Class `GetAllPlayerItems` · client → server · pairs with `ITEM_LIST` · codec `0x00185f64`/`0x00185ec8` · behaviour: ItemConnector 0x5a3c0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** At startup (online). Inventory shows a spinner until ITEM_LIST arrives.

**Server reply:** ITEM_LIST (mandatory, empty ok)

#### `GET_ALL_REQUEST`

Class `GetAllRequest` · client → server · pairs with `REQUEST_LIST` · codec `0x00186de4`/`0x00186d48` · behaviour: RequestConnector 0x6139c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** At startup.

**Server reply:** REQUEST_LIST with pending requests (empty ok)

#### `GET_BLACKLIST_PROFILES`

Class `GetBlackListProfiles` · client → server · pairs with `BLACKLIST_PROFILE_LIST` · codec `0x00188460`/`0x001883c4` · behaviour: BlacklistConnector 0x52b2c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** At startup.

**Server reply:** BLACKLIST_PROFILE_LIST (empty ok)

#### `GET_BLACK_BIRDS`

Class `GetBlackBirds` · client → server · pairs with `GET_BLACK_BIRDS_RESPONSE` · codec `0x00184dfc`/`0x00184cf4` · behaviour: LPMission6 keyboardReturned: 0x1f40f4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `email` | string |  |

**Client:** Mission 6: player types an email to receive the 'Blackbirds' mp3. Mission completes regardless.

**Server reply:** none needed

#### `GET_ONLINE_FRIENDS_STATUS`

Class `GetOnlineFriendsStatus` · client → server · pairs with `ONLINE_FRIENDS_STATUS_LIST` · codec `0x00187bd4`/`0x00187b38` · behaviour: FriendConnector 0x55154; FriendComponent cycle 0x203d00

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Every 20 s while the friend list is open (sent by requestOnlineFriendsID).

**Server reply:** ONLINE_FRIENDS_STATUS_LIST (empty ok)

#### `GET_PLAYER_ALL_ACHIEVEMENT`

Class `GetPlayerAllAchievements` · client → server · pairs with `PLAYER_ACHIEVEMENT_LIST` · codec `0x0019709c`/`0x00196fcc` · behaviour: AchievementConnector 0x505b4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** At startup with own playerID, and when viewing a profile's trophies.

**Server reply:** PLAYER_ACHIEVEMENT_LIST echoing targetPlayerID

#### `GET_PLAYER_PROFILE`

Class `GetPlayerProfile` · client → server · pairs with `PLAYER_PROFILE` · codec `0x0019428c`/`0x001941bc` · behaviour: PlayerConnector 0x5ef08; BaseProfileComponent 0x246ca8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** Opening any profile view (spinner 3).

**Server reply:** PLAYER_PROFILE (mandatory, else spinner never stops)

#### `GET_PLAYER_RATING_LIST`

Class `GetPlayerRatingList` · client → server · pairs with `PLAYER_RATING_LIST` · codec `0x0019338c`/`0x00193200` · behaviour: FriendConnector 0x54fc0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |
| 2 | `playerRatingList` | array<int32> |  |

**Client:** On opening any profile; playerRatingList is always [1,2,3,4,5].

**Server reply:** PLAYER_RATING_LIST

#### `GET_PROFANITY_WORDS_ENTITY`

Class `GetProfanityWordsEntity` · client → server · pairs with `PROFANITY_WORD_ENTITIES` · codec `0x00185338`/`0x0018529c` · behaviour: ProfanityConnector 0x15b86c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** At startup and during account creation if the word list is not ready.

**Server reply:** PROFANITY_WORD_ENTITIES (seed from lp.sqlite profanitywords)

#### `GOTO_APARTMENT`

Class `GoToApartment` · client → server · pairs with `APARTMENT_INFO` · codec `0x0018c788`/`0x0018c6b8` · behaviour: ApartmentConnector 0x52160

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `apartmentID` | int32 |  |

**Client:** Entering an apartment; apartmentID = first number of profile.apartmentIDList.

**Server reply:** APARTMENT_INFO (mandatory when visiting another player or when own data is missing)

#### `GUEST_LOGIN`

Class `GuestLogin` · client → server · pairs with `LOGIN_RESPONSE` · codec `0x001980e0`/`0x00198010` · behaviour: AccountConnector 0x4d1a4; AccountHandler cycle 0x2106e8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `version` | int32 | always 1 |

**Client:** Sent every 20 s in guest-reconnect mode (network mode 2) after an offline profile exists; this is the 'Call connect to server' seen after the character editor.

**Server reply:** LOGIN_RESPONSE with responseCode 5 (keeps the local profile, enters guest mode) or 0/4 (online mode).

#### `IM_BUSY`

Class `IMBusy` · server → client · codec `0x00191204`/`0x00191134` · behaviour: IMComponent 0x1c9a8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** 'The player is currently unavailable for chatting.'

#### `IM_MESSAGE`

Class `IMLPMessage` · both directions · codec `0x00190c34`/`0x00190aa8` · behaviour: IMConnector 0x57fd4, 0x581b4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |
| 2 | `message` | bytes |  |
| 3 | `timestamp` | datetime |  |

**Client:** Out: targetPlayerID = peer, message = opaque IMContentMessage bytes. In: relay with targetPlayerID = the sender and the bytes unchanged; dropped if no room is open.

**Server reply:** relay to peer

#### `IM_REJECT`

Class `IMReject` · server → client · codec `0x00191044`/`0x00190f74` · behaviour: IMComponent 0x1c204

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** Handler is empty; optional.

#### `IM_START`

Class `IMStart` · server → client · codec `0x00186c54`/`0x00186af4` · behaviour: IMComponent IMStart: 0x1e054

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerProfile` | PlayerProfile |  |

**Client:** Push to both parties after an accepted IM request; targetPlayerProfile = the other party. Opens a chat room (max 2).

#### `IM_STOP`

Class `IMStop` · both directions · codec `0x00190e84`/`0x00190db4` · behaviour: IMConnector 0x57f28; IMComponent 0x1c740

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** Out when the user closes a chat. In: 'The chat with %@ was closed'. Relay with targetPlayerID = the stopper.

**Server reply:** relay to peer

#### `ITEM_LIST`

Class `ItemList` · server → client · codec `0x0018e06c`/`0x0018de68` · behaviour: ItemConnector 0x5ae5c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `itemList` | array<Item> | realPlayerItemID != 0, tempPlayerItemID = 0 |

**Client:** Replaces the local PlayerItem table; pending local items (tempPlayerItemID != 0) are re-sent as new ADD_ITEMs.

#### `LOCATION_PLAYER_PROFILE`

Class `LocationPlayerProfile` · server → client · codec `0x001927ac`/`0x001925a8` · behaviour: AvatarComponent locationPlayerProfile: 0x3f750

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerProfileList` | array<PlayerProfile> | other online players in the location; exclude the requester |

**Client:** AvatarComponent spawns one static OtherPlayerAvatar per profile (deduped by playerID, self not filtered) at a random x. Dropped in locations 7, 9, 29, 31, 33, 34 and some mission states. No later movement or removal messages exist.

#### `LOGIN`

Class `Login` · client → server · pairs with `LOGIN_RESPONSE` · codec `0x00198354`/`0x001981d0` · behaviour: AccountConnector 0x4d4cc, 0x4d21c; LoginComponent 0x2be4c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `userName` | string |  |
| 2 | `password` | string | plaintext |
| 3 | `version` | int32 | always 1 |

**Client:** Sent by -[AccountConnector loginWithName:Password:Version:] (login form, version=1), loginPreviousAccount (every 20 s in online-reconnect mode, stored credentials). Password is plaintext.

**Server reply:** LOGIN_RESPONSE

#### `LOGIN_RESPONSE`

Class `LoginResponse` · server → client · codec `0x00197e9c`/`0x00197c6c` · behaviour: AccountConnector receiveNetworkMessage 0x4e30c; LoginComponent cycle 0x29cf0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | becomes the client's player ID (header of every later message) |
| 1 | `responseCode` | int32 | 0 ok, 1 bad credentials, 2/3 fail, 4 ok-no-profile, 5 ok-guest, 6 unknown |
| 2 | `account` | LPAccount |  |
| 3 | `profile` | PlayerProfile |  |

**Client:** Ignored if already logged in. responseCode 0 = success (profile.profileName must equal the login userName case-insensitively, else the client treats it as no answer and the spinner never stops); 1 = 'Invalid Name or Password'; 2,3 = silent return to form; 4 = success, account has no profile yet (create-profile UI); 5 = accepted (guest/keep local profile, runs startup process); 6 = stored only. Codes 0/4/5 store the account and set the header playerID as the client's player ID for all later messages. Code 0 also replaces the local profile with `profile`, then the client sends GAME_ID_UPDATE and (from the login form) ACCOUNT_UPDATE + GAME_ID_UPDATE. Then the offline buffer is replayed.

#### `LOGOUT`

Class `Logout` · server → client · codec `0x0018940c`/`0x00189370` · behaviour: AccountConnector 0x4e30c, logout 0x4d158

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Server-initiated kick: listeners log out and the client stops the network manager. The client builds a LOGOUT object but never sends it.

#### `MAIL_LIST`

Class `MailList` · server → client · codec `0x0018f3e8`/`0x0018f1e4` · behaviour: MailComponent 0x23cb8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mailList` | array<Mail> |  |

**Client:** Shown in list order (send newest first); mail from self shows as 'Jess'; unread mail from blocked players is auto-deleted.

#### `NEWS_LIST`

Class `NewsList` · server → client · codec `0x0018a1bc`/`0x00189fb8` · behaviour: MediaPlayerComponent 0x178574

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `newsList` | array<NewsContent> |  |

**Client:** Media-player news table; created parsed as YYYYMMDDhhmmss; item tappable if link longer than 7 chars.

#### `NEW_BULLETIN_MESSAGE`

Class `NewBulletinMessage` · server → client · codec `0x0018592c`/`0x001857cc` · behaviour: BulletinComponent 0x15c9a4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `message` | BulletinMessage |  |

**Client:** Optional push; inserted at top if locationID matches the open board.

#### `NEW_FRIEND_MADE`

Class `NewFriendMade` · server → client · codec `0x001888c4`/`0x00188764` · behaviour: FriendComponent 0x202930

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `friendProfile` | PlayerProfile |  |

**Client:** Push: adds friendProfile (the other player) unless already a friend; 'You are friends with %@ now.'

#### `NEW_MAIL`

Class `NewMail` · server → client · codec `0x001868b4`/`0x00186754` · behaviour: MailComponent 0x239b4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mail` | Mail |  |

**Client:** Push: 'Received new mail from %@'.

#### `NEW_NEWS`

Class `NewNews` · server → client · codec `0x0018505c`/`0x00184efc` · behaviour: NewsConnector 0x1a4a84

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `newsContent` | NewsContent |  |

**Client:** Matched but ignored.

#### `NOTIFICATION_LIST`

Class `NotificationList` · server → client · codec `0x00192424`/`0x00192220` · behaviour: NotificationConnector 0x5dc24

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `notificationList` | array<Notification> |  |

**Client:** Delegate handler is an empty stub; never needed.

#### `ONLINE_FRIENDS_STATUS_LIST`

Class `OnlineFriendsStatusList` · server → client · codec `0x00192cf4`/`0x00192af0` · behaviour: FriendComponent 0x2020ac

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerStatusList` | array<PlayerStatus> |  |

**Client:** Friends listed with status 0 stay 0, other status -> 3; unlisted friends -> 4 (offline).

#### `PLAYER_ACHIEVEMENT_LIST`

Class `PlayerAchievementList` · server → client · codec `0x00196e14`/`0x00196be4` · behaviour: PlayerMonitorComponent 0xd87d4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |
| 2 | `achievementList` | array<Achievement> |  |

**Client:** For own playerID this enables achievement awarding for the session.

#### `PLAYER_PROFILE`

Class `PlayerProfileResponse` · server → client · codec `0x001882d0`/`0x00188170` · behaviour: BaseProfileComponent playerProfileResponse: 0x2483dc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerProfile` | PlayerProfile |  |

**Client:** Shown in the open profile view; not matched to the request.

#### `PLAYER_PROFILE_UPDATE`

Class `PlayerProfileUpdate` · client → server · codec `0x0018807c`/`0x00187f1c` · behaviour: PlayerConnector updateCurrentProfileToServer 0x5ebcc

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerProfile` | PlayerProfile |  |

**Client:** Full copy of the self profile after any change (credits, reputation, avatar, location, Facebook link). This is how the server learns currency. No reply.

**Server reply:** none

#### `PLAYER_RANK`

Class `PlayerRank` · client → server · pairs with `PLAYER_RANK_RESPONSE` · codec `0x00190954`/`0x00190828` · behaviour: PlayerConnector 0x5e4cc; BaseProfileComponent requestRank 0x246290

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |
| 2 | `startWorldRank` | int32 |  |
| 3 | `endWorldRank` | int32 |  |

**Client:** Profile rank tab; startWorldRank=0, endWorldRank=19 (top 20).

**Server reply:** PLAYER_RANK_RESPONSE

#### `PLAYER_RANK_RESPONSE`

Class `PlayerRankResponse` · server → client · codec `0x00190638`/`0x00190398` · behaviour: BaseProfileComponent 0x24d064

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |
| 2 | `playerRank` | RankEntity |  |
| 3 | `rankList` | array<RankEntity> |  |

**Client:** Fills the world rank table (spinner 4). responseCode ignored. RankEntity.rankID must be a playerID (row highlight; 0 shows 'Player'), rankIndex is 0-based. playerRank is appended if the target is not in rankList.

#### `PLAYER_RATING_LIST`

Class `PlayerRatingList` · server → client · codec `0x0019307c`/`0x00192e78` · behaviour: PlayerRatingComponent 0x145d38

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerRatingCountList` | array<PlayerRatingCount> |  |

**Client:** Average stars = sum(rating*numOfRating)/sum(numOfRating). No target ID: replies must follow request order. Also feeds achievements for own profile.

#### `PRODUCT_LIST`

Class `ProductList` · server → client · codec `0x00191b5c`/`0x00191958` · behaviour: ProductConnector 0x60d64

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `productList` | array<Product> |  |

**Client:** Handled if pushed (replaces the open store's list) but never requested.

#### `PROFANITY_WORD_ENTITIES`

Class `ProfanityWordEntities` · server → client · codec `0x0018a544`/`0x0018a340` · behaviour: ProfanityFilter 0x15ad2c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `wordEntityList` | array<WordEntity> |  |

**Client:** Replaces the in-memory profanity list (category 0 substring, other = word boundary).

#### `REMOVE_FRIEND`

Class `RemoveFriend` · client → server · codec `0x00195854`/`0x00195784` · behaviour: FriendConnector 0x56d68

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** Removes locally first; no reply. Server should push FRIEND_REMOVED to the other player.

**Server reply:** none

#### `REMOVE_FROM_BLACKLIST`

Class `RemoveFromBlackList` · client → server · codec `0x00194884`/`0x001947b4` · behaviour: BlacklistConnector 0x5323c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |

**Client:** No reply.

**Server reply:** none

#### `REMOVE_TOP_FRIEND`

Class `RemoveTopFriend` · client → server · codec `0x00194f8c`/`0x00194ebc` · behaviour: FriendConnector 0x5694c

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `friendID` | int32 |  |

**Client:** Star toggle; no reply.

**Server reply:** none

#### `REQUEST_LIST`

Class `RequestList` · server → client · codec `0x001917d4`/`0x001915d0` · behaviour: FriendComponent 0x20a578; IMComponent 0x1c348; PokeComponent 0x1576a8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `rList` | array<Request> |  |

**Client:** Additive (never cleared). type 1 -> friend request UI; type 0 -> IM accept popup, auto-answers RETURN_RESPONSE (1 if blocked, 2 if busy or own status 0); type 3 -> '%@ has poked you.'. Each Request needs requestID, fromPlayerID and the sender's full fromPlayerProfile.

#### `RETURN_RESPONSE`

Class `ReturnResponse` · client → server · codec `0x0019144c`/`0x001912f4` · behaviour: RequestConnector 0x61430

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `requestID` | int32 |  |
| 2 | `requestType` | int32 |  |
| 3 | `responseCode` | int32 |  |
| 4 | `targetPlayerID` | int32 |  |

**Client:** Answer to a request: responseCode 0 accept, 1 reject, 2 busy (IM); targetPlayerID = original requester. The client never handles an incoming RETURN_RESPONSE: the server turns it into NEW_FRIEND_MADE / IM_START / IM_BUSY / IM_REJECT pushes.

**Server reply:** none

#### `SEARCH_FBFRIEND_RESULT_LIST`

Class `SearchFBFriendResultList` · server → client · codec `0x00195600`/`0x001953fc` · behaviour: FriendComponent 0x201e44

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `fbFriendProfileList` | array<PlayerProfile> |  |

**Client:** Fills the Facebook friend table.

#### `SEARCH_FB_FRIEND`

Class `SearchFBFriend` · client → server · pairs with `SEARCH_FBFRIEND_RESULT_LIST` · codec `0x00195e7c`/`0x00195ccc` · behaviour: PlayerConnector 0x5e680

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `name` | string |  |
| 2 | `fbFriendIDList` | array<string> |  |

**Client:** FriendComponent cycle with Facebook friend IDs (name empty).

**Server reply:** SEARCH_FBFRIEND_RESULT_LIST (empty list ok)

#### `SEARCH_PLAYER`

Class `SearchPlayer` · client → server · pairs with `SEARCH_PLAYER_RESULT_LIST` · codec `0x00188cc8`/`0x00188bc0` · behaviour: PlayerConnector 0x5e5d4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `name` | string |  |

**Client:** Friend search box (online mode).

**Server reply:** SEARCH_PLAYER_RESULT_LIST (mandatory, else 'waiting' text stays)

#### `SEARCH_PLAYER_RESULT_LIST`

Class `SearchPlayerResultList` · server → client · codec `0x0019624c`/`0x00196048` · behaviour: FriendComponent 0x201d04

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerProfileList` | array<PlayerProfile> |  |

**Client:** Fills the search table.

#### `SEND_MAIL`

Class `SendMail` · client → server · pairs with `SEND_MAIL_RESPONSE` · codec `0x0018fdc8`/`0x0018fbc4` · behaviour: MailConnector 0x5caa4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `targetPlayerID` | int32 |  |
| 2 | `chainID` | int32 |  |
| 3 | `message` | bytes |  |
| 4 | `subject` | string |  |
| 5 | `soundTrack` | bytes |  |

**Client:** chainID always 0; message = opaque serialized mail content (may include a gift); soundTrack empty unless a clip is attached. Missions also mail the player itself (targetPlayerID = self).

**Server reply:** SEND_MAIL_RESPONSE; push NEW_MAIL to the recipient

#### `SEND_MAIL_RESPONSE`

Class `SendMailResposne` · server → client · codec `0x0018fa50`/`0x0018f8ec` · behaviour: MailComponent 0x1fef8

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 | 0 sent, 1 recipient mailbox full, 2 failed |
| 2 | `targetplayerID` | int32 |  |
| 3 | `targetPlayerName` | string |  |

**Client:** Ignored if targetplayerID = self. 0 'Mail Sent', 1 'mailbox is full', 2 'Mail fail to send'.

#### `SEND_NOTIFICATION`

Class `SendNotification` · server → client · codec `0x00187a44`/`0x001878e4` · behaviour: NotificationConnector 0x5dc24

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `notification` | Notification |  |

**Client:** DO NOT SEND: the handler calls Notification selectors on the message object and will throw an unrecognized-selector exception. The client never sends it either.

#### `SEND_REQUEST`

Class `SendRequest` · client → server · pairs with `REQUEST_LIST` · codec `0x00186ff4`/`0x00186e94` · behaviour: RequestConnector 0x61568; FriendConnector pokePlayer 0x54d40

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `request` | Request |  |

**Client:** request.type: 0 IM chat request (isInstance 1), 1 friend request (isInstance 0), 3 poke (isInstance 1, client fills fromPlayerProfile), 2 sent once with target 0 by IMComponent init (unclear; ignore). requestID and fromPlayerProfile are not filled by the client.

**Server reply:** none to the sender; relay to the target as REQUEST_LIST

#### `UPDATE_APARTMENT_DATA`

Class `UpdateApartmentData` · client → server · codec `0x0018c9b0`/`0x0018c878` · behaviour: ApartmentConnector 0x519b0

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `apartmentID` | int32 |  |
| 2 | `apartmentData` | bytes |  |

**Client:** Apartment editor SAVE; apartmentID = self apartmentIDList. No reply; store per apartment.

**Server reply:** none

### Compiled in but unused by this client (wire layout only)

#### `ACHIEVEMENT_LIST`

Class `AchievementList` · server → client (inferred from name) · codec `0x00197390`/`0x0019718c`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `achievementList` | array<Achievement> |  |

**Client:** Never handled.

#### `ACHIEVEMENT_LOST`

Class `AchievementLost` · server → client (inferred from name) · codec `0x00196af4`/`0x00196a24`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerAchievementID` | int32 |  |

**Client:** Never handled.

#### `ADD_FIGHT_RESULT`

Class `AddFightResult` · client → server (inferred from name) · codec `0x0018b524`/`0x0018b420`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `npcID` | int32 |  |
| 2 | `result` | bool |  |

**Client:** Fight results are local only.

#### `CREW_BATTLE_UPDATE`

Class `CrewBattleUpdate` · server → client (inferred from name) · codec `0x0018749c`/`0x0018733c`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `updateData` | CrewBattleStatus |  |

**Client:** Crew battle is not wired up in this client.

#### `DELETE_PROFILE`

Class `DeleteProfile` · client → server (inferred from name) · codec `0x00198598`/`0x001984c8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerProfileID` | int32 |  |

**Client:** Implemented (PlayerConnector deleteProfile 0x5efe0) but no caller.

#### `FIGHT_RESULT_LIST`

Class `FightResultList` · server → client (inferred from name) · codec `0x0018b854`/`0x0018b650` · behaviour: FightConnector 0x973e4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `resultList` | array<FightResult> |  |

**Client:** FightConnector compares the type and does nothing.

#### `GET_ALL_ACHIEVEMENT`

Class `GetAllAchievements` · client → server (inferred from name) · pairs with `ACHIEVEMENT_LIST` · codec `0x001890b8`/`0x0018901c`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Never sent.

#### `GET_ALL_FIGHT_RESULT`

Class `GetAllFightResult` · client → server (inferred from name) · pairs with `FIGHT_RESULT_LIST` · codec `0x001855d0`/`0x00185534`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Fight results are local only.

#### `GET_ALL_MISSION_STATE`

Class `GetAllMissionState` · client → server (inferred from name) · pairs with `MISSION_STATE_LIST` · codec `0x00186304`/`0x00186268`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Missions are local only.

#### `GET_ALL_PRODUCT`

Class `GetAllProduct` · client → server (inferred from name) · pairs with `PRODUCT_LIST` · codec `0x00191db0`/`0x00191ce0`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `storeID` | int32 |  |

**Client:** Never sent: the store reads lp.sqlite.

#### `GET_BLACK_BIRDS_RESPONSE`

Class `GetBlackBirdsResponse` · server → client (inferred from name) · codec `0x00184bfc`/`0x00184b28`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `success` | bool |  |

**Client:** Never handled.

#### `GET_FIGHT_RESULT`

Class `GetFightResult` · client → server (inferred from name) · pairs with `FIGHT_RESULT_LIST` · codec `0x0018bb38`/`0x0018b9d8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `npcIDList` | array<int32> |  |

**Client:** Fight results are local only.

#### `GET_FRIEND_PROFILE_ENTRIES`

Class `GetFriendProfileEntries` · client → server (inferred from name) · pairs with `PLAYER_PROFILE_ENTRYLIST` · codec `0x00194098`/`0x00193f98`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `key` | int32 |  |
| 2 | `sortBy` | int32 |  |

**Client:** Never sent.

#### `GET_LOGS`

Class `GetLogs` · client → server (inferred from name) · pairs with `LOG_LIST` · codec `0x0018ede4`/`0x0018ece4`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `logType` | int32 |  |
| 2 | `limit` | int32 |  |

**Client:** No caller.

#### `GET_NPC_CONTENT`

Class `GetNPCContent` · client → server (inferred from name) · pairs with `NPC_CONTENT_LIST` · codec `0x0018c178`/`0x0018c0a8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `npcID` | int32 |  |

**Client:** Never sent.

#### `GET_ONLINE_FRIEND_ID`

Class `GetOnlineFriendsID` · client → server (inferred from name) · pairs with `ONLINE_FRIENDS_ID_LIST` · codec `0x00187d20`/`0x00187c84`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Never sent.

#### `GET_PROFANITY_WORDS`

Class `GetProfanityWords` · client → server (inferred from name) · pairs with `PROFANITY_WORDS` · codec `0x00185484`/`0x001853e8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Never sent.

#### `GET_SOUND_TRACK`

Class `GetSoundTrack` · client → server (inferred from name) · pairs with `SOUND_TRACK` · codec `0x00194dcc`/`0x00194cfc`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mediaID` | int32 |  |

**Client:** Never sent.

#### `GET_TOTAL_FIGHT`

Class `GetTotalFight` · client → server (inferred from name) · pairs with `TOTAL_FIGHT` · codec `0x0018571c`/`0x00185680`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Fight results are local only.

#### `JOIN_CREW_BATTLE`

Class `JoinCrewBattle` · client → server (inferred from name) · pairs with `JOIN_CREW_BATTLE_RESPONSE` · codec `0x00187834`/`0x00187798`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |

**Client:** Crew battle is not wired up in this client.

#### `JOIN_CREW_BATTLE_RESPONSE`

Class `JoinCrewBattleResponse` · server → client (inferred from name) · codec `0x00192130`/`0x00192060`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `responseCode` | int32 |  |

**Client:** Crew battle is not wired up in this client.

#### `LOG_LIST`

Class `LogList` · server → client (inferred from name) · codec `0x0018eb60`/`0x0018e95c`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerLogList` | array<PlayerLog> |  |

**Client:** No listener.

#### `MEDIA_RESPONSE`

Class `MediaResponse` · server → client (inferred from name) · codec `0x0018abd0`/`0x0018a9cc`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mediaList` | array<Media> |  |

**Client:** No listener exists.

#### `MISSION_STATE`

Class `MissionState` · client → server (inferred from name) · pairs with `MISSION_STATE_RESPOSNE` · codec `0x00186514`/`0x001863b4`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `playerMission` | PlayerMission |  |

**Client:** Missions are local only.

#### `MISSION_STATE_LIST`

Class `MissionStateList` · server → client (inferred from name) · codec `0x0018e7d8`/`0x0018e5d4`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `missionStateList` | array<PlayerMission> |  |

**Client:** Missions are local only.

#### `MISSION_STATE_RESPOSNE`

Class `MissionStateResponse` · server → client (inferred from name) · codec `0x0018e4b0`/`0x0018e3b0`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `tempPlayerMissionID` | int32 |  |
| 2 | `playerMissionID` | int32 |  |

**Client:** Missions are local only (type name misspelled in the binary).

#### `NPC_CONTENT_LIST`

Class `NPCContentList` · server → client (inferred from name) · codec `0x0018bef0`/`0x0018bcc0`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `npcID` | int32 |  |
| 2 | `contentList` | array<NPCContent> |  |

**Client:** Never handled.

#### `ONLINE_FRIENDS_ID_LIST`

Class `OnlineFriendsIDList` · server → client (inferred from name) · codec `0x00193a88`/`0x00193928`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `friendIDList` | array<int32> |  |

**Client:** Handlers are empty stubs.

#### `OPTION_ENTRY_UPDATE`

Class `OptionEntryUpdate` · client → server (inferred from name) · codec `0x0018d558`/`0x0018d420`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `key` | int32 |  |
| 2 | `value` | string |  |

**Client:** Never sent.

#### `PLAYER_OPTION_LIST`

Class `PlayerOptionList` · server → client (inferred from name) · codec `0x0018d29c`/`0x0018d098`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `optionList` | array<OptionEntry> |  |

**Client:** Never handled.

#### `PLAYER_PROFILE_ENTRYLIST`

Class `PlayerProfileEntryList` · server → client (inferred from name) · codec `0x00193e14`/`0x00193c10`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `entryList` | array<ProfileEntry> |  |

**Client:** Never handled.

#### `PLAYER_PROFILE_ENTRY_UPDATE`

Class `PlayerProfileEntryUpdate` · client → server (inferred from name) · codec `0x00188670`/`0x00188510`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `entry` | ProfileEntry |  |

**Client:** Never sent.

#### `PLAYER_TAP`

Class `PlayerTap` · client → server (inferred from name) · codec `0x00191f70`/`0x00191ea0`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `tapCount` | int32 |  |

**Client:** Crew battle is not wired up in this client.

#### `PROFANITY_WORDS`

Class `ProfanityWords` · server → client (inferred from name) · codec `0x0018a838`/`0x0018a6c8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `wordList` | array<string> |  |

**Client:** Never handled.

#### `REQUEST_ALL_MEDIA`

Class `RequestAllMedia` · client → server (inferred from name) · pairs with `MEDIA_RESPONSE` · codec `0x0018ae24`/`0x0018ad54` · behaviour: MediaConnector 0xa5450

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mediaType` | int32 |  |

**Client:** MediaConnector is never instantiated.

#### `REQUEST_MEDIA`

Class `RequestMedia` · client → server (inferred from name) · pairs with `MEDIA_RESPONSE` · codec `0x0018b074`/`0x0018af14` · behaviour: MediaConnector requestSingleMedia: 0xa5394

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mediaIDList` | array<int32> |  |

**Client:** MediaConnector is never instantiated.

#### `SEARCH_FRIEND`

Class `SearchFriend` · client → server (inferred from name) · pairs with `SEARCH_FRIEND_RESULT_LIST` · codec `0x00188ac0`/`0x001889b8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `name` | string |  |

**Client:** Never sent.

#### `SEARCH_FRIEND_RESULT_LIST`

Class `SearchFriendResultList` · server → client (inferred from name) · codec `0x00195b48`/`0x00195944`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `friendProfileList` | array<PlayerProfile> |  |

**Client:** Never handled.

#### `SOUND_TRACK`

Class `SoundTrack` · server → client (inferred from name) · codec `0x001944b4`/`0x0019437c`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `mediaID` | int32 |  |
| 2 | `soundTrack` | bytes |  |

**Client:** Never handled.

#### `START_CREW_BATTLE`

Class `StartCrewBattle` · server → client (inferred from name) · codec `0x00187698`/`0x00187590`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `battleTime` | datetime |  |

**Client:** Crew battle is not wired up in this client.

#### `STOP_CREW_BATTLE`

Class `StopCrewBattle` · server → client (inferred from name) · codec `0x00187248`/`0x001870e8`

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `finalResult` | CrewBattleStatus |  |

**Client:** Crew battle is not wired up in this client.

#### `TOTAL_FIGHT`

Class `TotalFight` · server → client (inferred from name) · codec `0x0018b2fc`/`0x0018b1fc` · behaviour: FightConnector 0x973e4

| # | Field | Type | Notes |
|---|---|---|---|
| 0 | `playerID` | int32 | LPBaseMessage header |
| 1 | `winCount` | int32 |  |
| 2 | `loseCount` | int32 |  |

**Client:** FightConnector compares the type and does nothing.

### Entities

#### `Achievement`

Codec `0x0019cbbc`/`0x0019ca28`

| # | Field | Type |
|---|---|---|
| 0 | `playerAchievementID` | int32 |
| 1 | `tempPlayerAchievementID` | int32 |
| 2 | `achievementID` | int32 |
| 3 | `achievementValue` | string |
| 4 | `timestamp` | datetime |

#### `Apartment`

Codec `0x0019ba1c`/`0x0019b8b0`

| # | Field | Type |
|---|---|---|
| 0 | `apartmentID` | int32 |
| 1 | `apartmentName` | string |
| 2 | `apartmentData` | bytes |
| 3 | `districtID` | int32 |

#### `BulletinMessage`

Codec `0x0019a4f0`/`0x0019a314`

| # | Field | Type |
|---|---|---|
| 0 | `bulletinMessageID` | int32 |
| 1 | `playerID` | int32 |
| 2 | `playerName` | string |
| 3 | `message` | string |
| 4 | `timestamp` | datetime |
| 5 | `locationID` | int32 |

#### `CrewBattleStatus`

Codec `0x0019a134`/`0x00199fbc`

| # | Field | Type |
|---|---|---|
| 0 | `district1TapCount` | int32 |
| 1 | `district2TapCount` | int32 |
| 2 | `district3TapCount` | int32 |
| 3 | `district4TapCount` | int32 |
| 4 | `district5TapCount` | int32 |
| 5 | `district6TapCount` | int32 |
| 6 | `selfTapCount` | int32 |

#### `FightResult`

Codec `0x00199c04`/`0x00199b2c`

| # | Field | Type |
|---|---|---|
| 0 | `npcID` | int32 |
| 1 | `winCount` | int32 |
| 2 | `loseCount` | int32 |

#### `FriendProfile`

Codec `0x0019b7a4`/`0x0019b654`

| # | Field | Type |
|---|---|---|
| 0 | `friendProfile` | PlayerProfile |
| 1 | `friendType` | int32 |

#### `GamePlayerInfoInternal`

Codec `0x00198cec`/`0x00198b9c` · not carried by any message

| # | Field | Type |
|---|---|---|
| 0 | `version` | int32 |
| 1 | `playerProfile` | PlayerProfile |

Not used by any message (local save format).

#### `Item`

Codec `0x0019acd4`/`0x0019ab40`

| # | Field | Type |
|---|---|---|
| 0 | `realPlayerItemID` | int32 |
| 1 | `tempPlayerItemID` | int32 |
| 2 | `itemID` | int32 |
| 3 | `itemType` | int32 |
| 4 | `description` | string |
| 5 | `applePurchased` | bool |

itemType = itemID / 1000. Server-issued items: realPlayerItemID unique and non-zero, tempPlayerItemID 0.

#### `LPAccount`

Codec `0x0019b380`/`0x0019b054`

| # | Field | Type |
|---|---|---|
| 0 | `accountID` | int32 |
| 1 | `optin` | bool |
| 2 | `userName` | string |
| 3 | `password` | string |
| 4 | `email` | string |
| 5 | `UDID` | string |
| 6 | `osVersion` | string |
| 7 | `deviceType` | string |
| 8 | `deviceToken` | bytes |
| 9 | `deviceTokenLastUpdate` | datetime |

#### `LPMailData`

Codec `0x00189bb8`/`0x0019ea18` · not carried by any message

| # | Field | Type |
|---|---|---|
| 0 | `type` | int32 |
| 1 | `giftID` | int32 |
| 2 | `text` | string |
| 3 | `data` | bytes |

Not used directly by any message; the Mail.message blob is a serialized mail content object (inferred to be this layout).

#### `Mail`

Codec `0x0019d8bc`/`0x0019d5c0`

| # | Field | Type |
|---|---|---|
| 0 | `mailID` | int32 |
| 1 | `receiverPlayerID` | int32 |
| 2 | `senderPlayerID` | int32 |
| 3 | `chainID` | int32 |
| 4 | `message` | bytes |
| 5 | `subject` | string |
| 6 | `mediaID` | int32 |
| 7 | `created` | datetime |
| 8 | `updated` | datetime |
| 9 | `senderName` | string |
| 10 | `hadRead` | bool |

#### `Media`

Codec `0x001998d4`/`0x00199644`

| # | Field | Type |
|---|---|---|
| 0 | `mediaID` | int32 |
| 1 | `type` | int32 |
| 2 | `mediaLink` | string |
| 3 | `thumbnailLink` | string |
| 4 | `mediaName` | string |
| 5 | `mediaDescription` | string |
| 6 | `created` | datetime |
| 7 | `mediaToken` | string |

#### `MissionData`

Codec `0x0019c124`/`0x0019bfb8` · not carried by any message

| # | Field | Type |
|---|---|---|
| 0 | `playerMissionID` | int32 |
| 1 | `missionID` | int32 |
| 2 | `missionState` | int32 |
| 3 | `missionData` | bytes |
| 4 | `finished` | bool |

Not used by any message (local only).

#### `NPCContent`

Codec `0x00199e68`/`0x00199d24`

| # | Field | Type |
|---|---|---|
| 0 | `npcContentID` | int32 |
| 1 | `content` | string |
| 2 | `created` | datetime |

#### `NewsContent`

Codec `0x00199488`/`0x001992b4`

| # | Field | Type |
|---|---|---|
| 0 | `newsID` | int32 |
| 1 | `created` | datetime |
| 2 | `subject` | string |
| 3 | `content` | string |
| 4 | `link` | string |

#### `Notification`

Codec `0x0019bd98`/`0x0019bba4`

| # | Field | Type |
|---|---|---|
| 0 | `notificationID` | int32 |
| 1 | `content` | string |
| 2 | `timestamp` | datetime |
| 3 | `notificationRepresentation` | int32 |
| 4 | `notificationType` | int32 |
| 5 | `receiverID` | int32 |
| 6 | `isInstant` | bool |

#### `OptionEntry`

Codec `0x0019a7cc`/`0x0019a6dc`

| # | Field | Type |
|---|---|---|
| 0 | `key` | int32 |
| 1 | `value` | string |

#### `PlayerLog`

Codec `0x0019c870`/`0x0019c6dc`

| # | Field | Type |
|---|---|---|
| 0 | `logID` | int32 |
| 1 | `logType` | int32 |
| 2 | `logValue` | string |
| 3 | `logTimestamp` | datetime |
| 4 | `locationID` | int32 |

#### `PlayerMission`

Codec `0x0019c4bc`/`0x0019c2d4`

| # | Field | Type |
|---|---|---|
| 0 | `tempPlayerMissionID` | int32 |
| 1 | `playerMissionID` | int32 |
| 2 | `missionID` | int32 |
| 3 | `playerID` | int32 |
| 4 | `timestamp` | datetime |
| 5 | `state` | string |
| 6 | `finished` | bool |

#### `PlayerProfile`

Codec `0x0019e100`/`0x0019dbb8`

| # | Field | Type |
|---|---|---|
| 0 | `playerID` | int32 |
| 1 | `accountID` | int32 |
| 2 | `profileName` | string |
| 3 | `gender` | int32 |
| 4 | `arrayOfBodyPartIndex` | string |
| 5 | `FBID` | string |
| 6 | `FBProfilePicLink` | string |
| 7 | `status` | int32 |
| 8 | `hearts` | int32 |
| 9 | `credits` | int32 |
| 10 | `creationTime` | datetime |
| 11 | `previousLogin` | datetime |
| 12 | `districtID` | int32 |
| 13 | `locationID` | int32 |
| 14 | `currentLocatedApartmentID` | int32 |
| 15 | `isOnline` | bool |
| 16 | `playerVote` | int32 |
| 17 | `reputationRank` | int32 |
| 18 | `reputation` | int32 |
| 19 | `apartmentIDList` | string |
| 20 | `mediaID` | int32 |
| 21 | `receiveNotification` | bool |
| 22 | `isGuest` | bool |

status: 3 = available (default for new profiles), 0 = busy (auto-declines IM). gender != 0 is male (inferred). arrayOfBodyPartIndex: comma-separated, element 0 = skin index, then item IDs per slot. apartmentIDList: comma-separated apartment IDs, first = own apartment.

#### `PlayerRatingCount`

Codec `0x0019af64`/`0x0019aeb4`

| # | Field | Type |
|---|---|---|
| 0 | `rating` | int32 |
| 1 | `numOfRating` | int32 |

#### `PlayerStatus`

Codec `0x0019d4d0`/`0x0019d420`

| # | Field | Type |
|---|---|---|
| 0 | `playerID` | int32 |
| 1 | `status` | int32 |

#### `Product`

Codec `0x0019d114`/`0x0019cd74`

| # | Field | Type |
|---|---|---|
| 0 | `productID` | int32 |
| 1 | `productDescription` | string |
| 2 | `productName` | string |
| 3 | `productThumbnail` | string |
| 4 | `appleProductIdentifier` | string |
| 5 | `price` | int32 |
| 6 | `storeID` | int32 |
| 7 | `amount` | int32 |
| 8 | `itemList` | array<Item> |

#### `ProfileEntry`

Codec `0x0019a9f8`/`0x0019a8e0`

| # | Field | Type |
|---|---|---|
| 0 | `playerID` | int32 |
| 1 | `key` | int32 |
| 2 | `value` | string |

#### `RankEntity`

Codec `0x0019913c`/`0x00198ff8`

| # | Field | Type |
|---|---|---|
| 0 | `rankID` | int32 |
| 1 | `rankName` | string |
| 2 | `reputation` | int32 |
| 3 | `rankIndex` | int32 |

rankID carries a playerID; rankIndex is 0-based.

#### `Request`

Codec `0x0019e840`/`0x0019e648`

| # | Field | Type |
|---|---|---|
| 0 | `requestID` | int32 |
| 1 | `fromPlayerID` | int32 |
| 2 | `targetPlayerID` | int32 |
| 3 | `type` | int32 |
| 4 | `fromPlayerProfile` | PlayerProfile |
| 5 | `isInstance` | bool |

type 0 IM, 1 friend, 3 poke. Server fills requestID and fromPlayerProfile when relaying.

#### `WordEntity`

Codec `0x00198ee4`/`0x00198df8`

| # | Field | Type |
|---|---|---|
| 0 | `word` | string |
| 1 | `category` | int32 |

## Server-provided data and local seed data

| Data | Origin in the original game | Seed for a replacement server |
|---|---|---|
| Store catalogue and prices | Local `lp.sqlite` (`product`, `productitem`, `store`). `GET_ALL_PRODUCT` is never sent. | Same tables: the server only needs `productitem` to build `BUY_PRODUCT_RESPONSE.itemList` (and `product.ingame_price` if it wants to audit credits). |
| Profanity list | Local `profanitywords`, replaced in memory by `PROFANITY_WORD_ENTITIES` | `profanitywords` (`word`, `category`; 71 rows category 0, 124 rows category 1). |
| Reserved names | Local `BlockedNames` (271 rows), checked client-side | Reuse server-side if desired. |
| News | Server (`NEWS_LIST`) | None in the bundle; write your own. |
| Achievements list | Client-side IDs 22000–22014 with names in `Localizable.strings` (`ACHIEVEMENT NAME 220xx`) | The server only stores what clients report. |
| NPC content, sound tracks, media | Messages exist but are never used; assets ship in the bundle (`*.mp3`, `*.m4v`, NPC `*.txt` state machines) | Not needed. |
| Ranks and ratings | Server-computed from player data | Compute from stored `reputation` and ratings. |
| Missions | Local `Mission` table (10 rows, placeholder descriptions) and `PlayerMission` | Not needed. |

### `lp.sqlite` schema

| Table | Rows | Columns |
|---|---|---|
| `product` | 295 | `product_id, apple_product_identifier ('0' everywhere), ingame_price, description, store_id, amount, sold_count, enabled, created, product_name, product_thumbnail_link` |
| `productitem` | 293 | `product_item_id, product_id, item_id` (item IDs 1006–20000; `item_id / 1000` is the item type) |
| `store` | 29 | `store_id, name, description` (only stores 11, 13, 14, 15, 16, 21, 23, 29 have products) |
| `profanitywords` | 195 | `pwid, word, category, enabled` |
| `BlockedNames` | 271 | `name_id, name` |
| `Mission` | 10 | `mission_id, name, description` |
| `Achievement` | 0 | `playerAchievementID, tempPlayerAchievementID, achievementID` |
| `PlayerItem` | 0 | `player_item_id, temp_player_item_id, item_id, item_type, apple_purchased` |
| `PlayerMission` | 0 | `player_mission_id, mission_id, mission_state, mission_data, finished` |
| `PlayerMail` | 0 | `player_mail_id, sender_player_id, receiver_player_id, chain_id, content, read, subject, media_id, created, updated, senderName, contentSize` |
| `Friends`, `Blacklist` | 0 | cached `PlayerProfile` columns plus `topfriend`, `hearts`, `player_vote` |
| `FightResult` | 0 | `npc_id, win_count, lose_count` |
| `PlayerInternalData` | 0 | `DataName, DataType, Data` |

The empty tables are per-player client caches; their column names mirror the original server database and are a good model for server tables.

## HTTP side channels

| Channel | Format | Recommendation |
|---|---|---|
| Push-notification registration (`PushNotificationHandler`, `NetworkManager`) | `POST` to `http://lp110.m-botme.com/apnlp/incomingClientRequest`, `GCEvent` binary encoded by `NMBinaryParser`/`PNMessageParser`, key `PN_ENCRYPTOR_KEY_ALIFE`; only `DEVICE_TOKEN_UPDATE_REQUEST` is sent (`CoreUIPanel` @ `0x10fe4c`) | Not sent without an APNs device token, which an emulator does not have. Stub or ignore. |
| Leaderboard (`HighScoreManager`, used only by the `Facebook` class) | `HSRegistrationRequest`, `HSRankRequest`, `HSScoreHistoryRequest`, `HSSocialNetworkRankRequest` over `NetworkManager` | The shipped build points at `192.168.1.20` (development flag), so it never worked in production. Stub. |
| Facebook Connect (`FBRequest`, `GCFacebookManager`) | Old REST API, `api.facebook.com/restserver.php` | `GCFacebookManager setIsBlocked:YES` is called at init. Dead; stub. |
| Twitter (`GCTwitterAuth`, `GCTwitterManager`) | OAuth 1.0 against `api.twitter.com/1/*.xml` | Dead API. Stub. |
| Web links | Terms of use, ads, `botme.com/ref/19xx` | Local HTML copies exist; links can fail harmlessly. |

The game channel itself uses no `NSURLConnection`; the side channels do.

## Game-side patches and emulator requirements

1. **Server address.** `http://lp110.m-botme.com/lp/httpGDServlet` is a C string literal in `__cstring` (armv7 VA `0x317c3c`; fat-file offsets 3251260 and 9718844 for the two slices). Either resolve `lp110.m-botme.com` to the replacement server in the emulator (no binary change), or patch the string in place with a URL of the same or shorter length followed by a NUL. The client takes host, port (default 80) and path from it, so `http://host:port/path` works.
2. **Sockets.** The client opens raw TCP with `CFStreamCreatePairWithSocketToHost` and speaks HTTP/1.1 itself. touchHLE must implement socket `CFReadStream`/`CFWriteStream` (open, read, write, events on the run loop). For a browser build, a bridge is needed: each request is a complete HTTP exchange on its own connection (`Connection: Close`/auto-close), so the emulator can map one CFStream pair to one `fetch()` or one WebSocket-proxied TCP connection.
3. **Reachability.** `HTTPClient init` asks `Reachability internetConnectionStatus`; if it reports "not reachable" the client immediately reports a disconnect. The emulator must report Wi-Fi reachability.
4. **String decoding.** The client decodes strings with `+[NSString stringWithCString:length:]`. The emulator's implementation must accept the server's bytes (ASCII is safest).
5. **No piracy trap needed.** If `Info.plist` contains `SignerIdentity`, the client only sends an `ADD_LOG` report; it does not block play.
6. **Known client bugs to tolerate:** buying in online-reconnect mode leaves a spinner running; visiting another player's apartment writes that apartment's data into the visitor's own apartment file if the visitor has none.

No authentication bypass is needed: the server decides what `LOGIN` accepts.

## Open questions

- `LOGIN_RESPONSE` codes 2, 3 and 6: the client distinguishes them but shows nothing specific. Their original meaning is unknown.
- `SEND_REQUEST` with `type=2` and target 0, sent once by `IMComponent init` (`0x1f658`): purpose unknown; the decompiled arguments may be wrong. Ignore it.
- The exact slot mapping of `arrayOfBodyPartIndex` item IDs (`mappingItemIDtoArrayIndex:` @ `0x264e80`, switch values lost in decompilation). A server only needs to store and echo the string.
- Timeouts: the `NetworkLoading` spinner duration and the `HTTPClient` write timeout (6000 or 18000, unit unclear) were not resolved.
- Whether `BLOCK_PLAYER` should also end a friendship: the client does not decide.
- The meaning of the `HTTP-Response-Status` header and the PDU type byte values other than 2.
- Server timezone for `datetime` fields.
- Nothing was verified against a live server or a packet capture; the original servers are gone. The framing and codec were cross-checked only by a round-trip test of the catalogue.

## Reproducing the analysis

The decompiled code is derived from the proprietary binary and is kept outside the repository. The analysis used Ghidra 12.1.4 headless on the armv7 slice (extracted with LIEF), a Java post-script that decompiles every ObjC method grouped by class, and a resolver that annotates ARM PC-relative literals with selector, class, string, ivar and static-symbol names. Wire layouts were extracted mechanically from the `LPServerMessageParser` encoders and cross-checked against the decoders; all 157 classes agree. Client behaviour was read from the connector classes (`*Connector`, `AccountHandler`, `LoginComponent`, `NetworkHandler`, `StreamNetworkManager`, `HTTPServerChannel`, `HTTPClient`).
