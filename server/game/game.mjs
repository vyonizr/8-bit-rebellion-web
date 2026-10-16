// Replacement for the 8-Bit Rebellion game server (lp110.m-botme.com/lp/httpGDServlet).
// The protocol and every rule implemented here are described in docs/protocol.md.
import crypto from "node:crypto";
import {
  CHANNEL_SETUP,
  decodePdu,
  encodeContainer,
  encodePdu,
  formatDate,
} from "./codec.mjs";

/** Sessions that haven't polled for this long are dropped. The client polls every 12 s. */
const SESSION_TIMEOUT_MS = 60_000;
/** Apartment IDs handed to clients are this plus the owner's player ID. */
const APARTMENT_ID_BASE = 100_000;
const DISTRICTS = ["Central Park", "The Beach", "The Park", "SoHo", "Casino Row", "The West Side", "Downtown"];

const REQUEST_IM = 0;
const REQUEST_FRIEND = 1;
const REQUEST_POKE = 3;

class Session {
  constructor() {
    this.cookie = crypto.randomBytes(16).toString("hex");
    this.sessionId = crypto.randomBytes(8).toString("hex");
    this.outbox = [];
    this.playerId = 0;
    this.accountId = 0;
    this.location = -1;
    /** Players already shown to this session in its current location. */
    this.seen = new Set();
    this.chatPartners = new Set();
    this.lastSeen = Date.now();
  }
}

export class Game {
  constructor(store, { log = console.log, trace = false } = {}) {
    this.store = store;
    this.log = log;
    this.trace = trace;
    this.sessions = new Map(); // cookie -> Session
    this.online = new Map(); // playerId -> Session
    if (store.news().length === 0) {
      store.addNews(
        "Welcome back to the Rebellion",
        "The 8-Bit Rebellion servers are online again. Have fun!",
        formatDate(),
      );
    }
  }

  // HTTP layer

  /** Handle one request to the game servlet. `body` is the request body. */
  handleHttp(req, res, body) {
    this.expireSessions();
    const cookie = /JSESSIONID=([0-9a-f]+)/.exec(req.headers.cookie ?? "")?.[1];

    if (req.headers.action === "INIT_CHANNEL") {
      const session = new Session();
      this.sessions.set(session.cookie, session);
      // Skip the optional first half of the handshake: a non-zero status
      // with a session ID completes it.
      this.send(session, CHANNEL_SETUP, { channelStatus: 1 });
      res.writeHead(200, {
        "Set-Cookie": `JSESSIONID=${session.cookie}; Path=/`,
        "Content-Length": 0,
      });
      res.end();
      return;
    }

    const session = cookie && this.sessions.get(cookie);
    if (!session) {
      res.writeHead(200, { SESSION_VALID: "FALSE", "Content-Length": 0 });
      res.end();
      return;
    }
    session.lastSeen = Date.now();

    if (req.method === "POST" && body.length > 0) {
      try {
        const { messages } = decodePdu(body);
        for (const { type, msg } of messages) this.handleMessage(session, type, msg);
      } catch (err) {
        this.log(`Bad message from session ${session.cookie}: ${err.stack}`);
      }
    }

    const reply = encodeContainer(session.outbox.splice(0));
    res.writeHead(200, {
      SESSION_VALID: "TRUE",
      "Content-Type": "application/octet-stream",
      "Content-Length": reply.length,
    });
    res.end(reply);
  }

  expireSessions() {
    const now = Date.now();
    for (const [cookie, session] of this.sessions) {
      if (now - session.lastSeen > SESSION_TIMEOUT_MS) {
        this.sessions.delete(cookie);
        this.goOffline(session);
      }
    }
  }

  send(session, type, msg) {
    if (this.trace) this.log(`player ${session.playerId} <- ${type}`);
    session.outbox.push(encodePdu(type, session.sessionId, { playerID: session.playerId, ...msg }));
  }

  /** Send to a player if they're online. Returns whether they were. */
  sendToPlayer(playerId, type, msg) {
    const session = this.online.get(playerId);
    if (session) this.send(session, type, msg);
    return !!session;
  }

  // Login state

  goOnline(session, playerId) {
    if (session.playerId && session.playerId !== playerId) this.goOffline(session);
    session.playerId = playerId;
    const previous = this.online.get(playerId);
    if (previous && previous !== session) this.goOffline(previous);
    this.online.set(playerId, session);
  }

  goOffline(session) {
    if (!session.playerId || this.online.get(session.playerId) !== session) return;
    this.online.delete(session.playerId);
    for (const partner of session.chatPartners) {
      this.sendToPlayer(partner, "IM_STOP", { targetPlayerID: session.playerId });
    }
    session.chatPartners.clear();
  }

  /** A player's profile as other clients should see it. */
  profile(playerId) {
    const player = this.store.player(playerId);
    if (!player) return { playerID: playerId, status: 3 };
    return {
      ...player.profile,
      playerID: playerId,
      accountID: player.account_id ?? 0,
      apartmentIDList: String(APARTMENT_ID_BASE + playerId),
      isOnline: this.online.has(playerId),
      isGuest: !!player.is_guest,
      status: player.profile.status ?? 3,
    };
  }

  account(accountId) {
    const row = accountId && this.store.accountById(accountId);
    if (!row) return {};
    return { ...JSON.parse(row.data), accountID: row.account_id, password: "" };
  }

  // Messages

  handleMessage(session, type, m) {
    if (this.trace) this.log(`player ${session.playerId} -> ${type}`);
    const handler = this.handlers[type];
    if (!handler) {
      this.log(`Unhandled message ${type} from player ${session.playerId}`);
      return;
    }
    // Messages other than login need a logged-in player.
    if (!session.playerId && !PRE_LOGIN.has(type)) {
      this.log(`Ignoring ${type} before login`);
      return;
    }
    handler.call(this, session, m);
  }

  handlers = {
    [CHANNEL_SETUP](s) {
      this.send(s, CHANNEL_SETUP, { channelStatus: 1 });
    },

    GUEST_LOGIN(s, m) {
      const existing = m.playerID > 0 && this.store.player(m.playerID);
      const playerId =
        existing && existing.is_guest ? m.playerID : this.store.createPlayer(null, true, { status: 3 });
      this.goOnline(s, playerId);
      // 5 keeps the client's local profile and enters guest mode.
      this.send(s, "LOGIN_RESPONSE", {
        playerID: playerId,
        responseCode: 5,
        account: {},
        profile: this.profile(playerId),
      });
    },

    CREATE_ACCOUNT(s, { account }) {
      let responseCode = 0;
      if (!account.userName || this.store.accountByName(account.userName)) responseCode = 1;
      else if (this.store.emailTaken(account.email)) responseCode = 2;
      else this.store.createAccount(account);
      this.send(s, "CREATE_ACCOUNT_RESPONSE", { responseCode });
    },

    LOGIN(s, { userName, password }) {
      const row = this.store.accountByName(userName);
      if (!row || row.password !== password) {
        this.send(s, "LOGIN_RESPONSE", { playerID: 0, responseCode: 1, account: {}, profile: {} });
        return;
      }
      s.accountId = row.account_id;
      const player = this.store.playerByAccount(row.account_id);
      if (!player) {
        // 4: the account has no profile yet, so the client creates one.
        this.send(s, "LOGIN_RESPONSE", {
          playerID: 0,
          responseCode: 4,
          account: this.account(row.account_id),
          profile: {},
        });
        return;
      }
      this.goOnline(s, player.player_id);
      // The client requires the profile name to match the user name.
      const profile = { ...this.profile(player.player_id), profileName: row.user_name };
      profile.previousLogin = formatDate();
      this.send(s, "LOGIN_RESPONSE", {
        playerID: player.player_id,
        responseCode: 0,
        account: this.account(row.account_id),
        profile,
      });
    },

    FORGOT_PASSWORD(s) {
      // There's no mail service; report the address as unknown.
      this.send(s, "FORGOT_PASSWORD_RESPONSE", { responseCode: 1 });
    },

    CREATE_PROFILE(s, { profile }) {
      if (!s.accountId) {
        this.log("CREATE_PROFILE without a logged-in account");
        return;
      }
      const userName = this.store.accountById(s.accountId).user_name;
      const playerId = this.store.createPlayer(s.accountId, false, {
        ...profile,
        profileName: userName,
        creationTime: formatDate(),
      });
      this.goOnline(s, playerId);
      this.send(s, "CREATE_PROFILE_RESPONSE", {
        playerID: playerId,
        responseCode: 0,
        playerProfile: this.profile(playerId),
      });
    },

    ACCOUNT_UPDATE(s, { account }) {
      if (s.accountId) this.store.updateAccountData(s.accountId, { ...account, password: undefined });
      this.send(s, "ACCOUNT_UPDATE_RESPONSE", { responseCode: 0, account: this.account(s.accountId) });
    },

    GAME_ID_UPDATE() {},
    ADD_LOG() {},
    GET_BLACK_BIRDS() {},

    PLAYER_PROFILE_UPDATE(s, { playerProfile }) {
      const player = this.store.player(s.playerId);
      // Registered players keep their account name as profile name.
      const profileName = player?.is_guest ? playerProfile.profileName : player?.profile.profileName;
      this.store.saveProfile(s.playerId, { ...playerProfile, profileName });
    },

    GET_PLAYER_PROFILE(s, { targetPlayerID }) {
      this.send(s, "PLAYER_PROFILE", { playerProfile: this.profile(targetPlayerID) });
    },

    // Presence

    CHANGE_LOCATION(s, { locationID }) {
      s.location = locationID;
      s.seen.clear();
      const me = this.profile(s.playerId);
      const here = [];
      for (const [playerId, other] of this.online) {
        if (other === s || other.location !== locationID) continue;
        const profile = this.profile(playerId);
        if (!profile.profileName) continue;
        here.push(profile);
        s.seen.add(playerId);
        // Show the newcomer to players already here.
        if (me.profileName && !other.seen.has(s.playerId)) {
          other.seen.add(s.playerId);
          this.send(other, "LOCATION_PLAYER_PROFILE", { playerProfileList: [me] });
        }
      }
      this.send(s, "LOCATION_PLAYER_PROFILE", { playerProfileList: here });
    },

    // Apartments

    GOTO_APARTMENT(s, { apartmentID }) {
      const owner = apartmentID > APARTMENT_ID_BASE ? apartmentID - APARTMENT_ID_BASE : s.playerId;
      this.send(s, "APARTMENT_INFO", {
        apartment: {
          apartmentID,
          apartmentName: this.profile(owner).profileName ?? "",
          // A single zero byte is the client's own "empty apartment".
          apartmentData: this.store.apartment(owner) ?? Buffer.from([0]),
          districtID: 0,
        },
        playerProfileList: [],
      });
    },

    UPDATE_APARTMENT_DATA(s, { apartmentData }) {
      this.store.setApartment(s.playerId, apartmentData);
    },

    // Items and the store

    GET_ALL_PLAYER_ITEMS(s) {
      const itemList = this.store.items(s.playerId).map((row) => ({
        realPlayerItemID: row.real_id,
        tempPlayerItemID: 0,
        itemID: row.item_id,
        itemType: row.item_type,
        description: row.description,
      }));
      this.send(s, "ITEM_LIST", { itemList });
    },

    ADD_ITEM(s, { item }) {
      const realPlayerItemID = this.store.addItem(s.playerId, item);
      this.send(s, "ADD_ITEM_RESPONSE", { item: { ...item, realPlayerItemID } });
    },

    DELETE_ITEM(s, { playerItemID }) {
      this.store.deleteItem(s.playerId, playerItemID);
    },

    BUY_PRODUCT(s, { itemList }) {
      const bought = itemList.map((item) => ({
        ...item,
        realPlayerItemID: this.store.addItem(s.playerId, item),
        tempPlayerItemID: 0,
      }));
      this.send(s, "BUY_PRODUCT_RESPONSE", { responseCode: 0, itemList: bought });
    },

    // Achievements

    ACHIEVEMENT_GAIN(s, { achievement }) {
      const playerAchievementID = this.store.addAchievement(s.playerId, achievement);
      this.send(s, "ACHIEVEMENT_GAIN_RESPONSE", {
        responseCode: 0,
        achievement: { ...achievement, playerAchievementID },
      });
    },

    GET_PLAYER_ALL_ACHIEVEMENT(s, { targetPlayerID }) {
      const achievementList = this.store.achievements(targetPlayerID).map((row) => ({
        playerAchievementID: row.id,
        tempPlayerAchievementID: 0,
        achievementID: row.achievement_id,
        achievementValue: row.value,
        timestamp: row.timestamp,
      }));
      this.send(s, "PLAYER_ACHIEVEMENT_LIST", { targetPlayerID, achievementList });
    },

    // Friends, requests and chat

    GET_ALL_FRIENDS_PROFILE(s) {
      const friendProfileList = this.store.friendRows(s.playerId).map((row) => ({
        friendProfile: this.profile(row.friend_id),
        friendType: row.top ? 1 : 0,
      }));
      this.send(s, "FRIEND_PROFILE_LIST", { friendProfileList });
    },

    GET_ONLINE_FRIENDS_STATUS(s) {
      const playerStatusList = this.store
        .friends(s.playerId)
        .filter((id) => this.online.has(id))
        .map((id) => ({ playerID: id, status: this.profile(id).status }));
      this.send(s, "ONLINE_FRIENDS_STATUS_LIST", { playerStatusList });
    },

    ADD_TOP_FRIEND(s, { friendID }) {
      this.store.setTopFriend(s.playerId, friendID, true);
    },
    REMOVE_TOP_FRIEND(s, { friendID }) {
      this.store.setTopFriend(s.playerId, friendID, false);
    },

    REMOVE_FRIEND(s, { targetPlayerID }) {
      this.store.removeFriends(s.playerId, targetPlayerID);
      this.sendToPlayer(targetPlayerID, "FRIEND_REMOVED", { targetPlayerID: s.playerId });
    },

    GET_BLACKLIST_PROFILES(s) {
      const profileList = this.store.blocked(s.playerId).map((id) => this.profile(id));
      this.send(s, "BLACKLIST_PROFILE_LIST", { profileList });
    },
    BLOCK_PLAYER(s, { targetPlayerID }) {
      this.store.block(s.playerId, targetPlayerID);
    },
    REMOVE_FROM_BLACKLIST(s, { targetPlayerID }) {
      this.store.unblock(s.playerId, targetPlayerID);
    },

    GET_ALL_REQUEST(s) {
      const rList = this.store.requestsFor(s.playerId).map((row) => this.requestEntity(row));
      this.send(s, "REQUEST_LIST", { rList });
    },

    SEND_REQUEST(s, { request }) {
      const target = request.targetPlayerID;
      if (!target || this.store.blocked(target).includes(s.playerId)) return;
      if (request.type === REQUEST_FRIEND) {
        const requestId = this.store.addRequest(s.playerId, target, REQUEST_FRIEND);
        this.sendToPlayer(target, "REQUEST_LIST", {
          rList: [this.requestEntity(this.store.request(requestId))],
        });
      } else if (request.type === REQUEST_IM || request.type === REQUEST_POKE) {
        // Chat requests and pokes are only delivered to online players. The
        // request ID encodes the sender so the answer can be routed back.
        const relayed = {
          requestID: s.playerId,
          fromPlayerID: s.playerId,
          targetPlayerID: target,
          type: request.type,
          fromPlayerProfile: this.profile(s.playerId),
          isInstance: true,
        };
        const delivered = this.sendToPlayer(target, "REQUEST_LIST", { rList: [relayed] });
        if (!delivered && request.type === REQUEST_IM) {
          this.send(s, "IM_BUSY", { targetPlayerID: target });
        }
      }
    },

    RETURN_RESPONSE(s, { requestID, requestType, responseCode, targetPlayerID }) {
      if (requestType === REQUEST_FRIEND) {
        const request = this.store.request(requestID);
        if (!request || request.target_id !== s.playerId) return;
        this.store.deleteRequest(requestID);
        if (responseCode === 0) {
          this.store.addFriends(request.from_id, s.playerId);
          this.sendToPlayer(request.from_id, "NEW_FRIEND_MADE", { friendProfile: this.profile(s.playerId) });
          this.send(s, "NEW_FRIEND_MADE", { friendProfile: this.profile(request.from_id) });
        }
      } else if (requestType === REQUEST_IM) {
        const from = targetPlayerID || requestID;
        if (responseCode === 0) {
          s.chatPartners.add(from);
          this.online.get(from)?.chatPartners.add(s.playerId);
          this.sendToPlayer(from, "IM_START", { targetPlayerProfile: this.profile(s.playerId) });
          this.send(s, "IM_START", { targetPlayerProfile: this.profile(from) });
        } else if (responseCode === 1) {
          this.sendToPlayer(from, "IM_REJECT", { targetPlayerID: s.playerId });
        } else {
          this.sendToPlayer(from, "IM_BUSY", { targetPlayerID: s.playerId });
        }
      }
    },

    IM_MESSAGE(s, { targetPlayerID, message }) {
      this.sendToPlayer(targetPlayerID, "IM_MESSAGE", {
        targetPlayerID: s.playerId,
        message,
        timestamp: formatDate(),
      });
    },

    IM_STOP(s, { targetPlayerID }) {
      s.chatPartners.delete(targetPlayerID);
      this.online.get(targetPlayerID)?.chatPartners.delete(s.playerId);
      this.sendToPlayer(targetPlayerID, "IM_STOP", { targetPlayerID: s.playerId });
    },

    // Mail

    GET_ALL_MAIL(s) {
      const mailList = this.store.mailFor(s.playerId).map((row) => this.mailEntity(row));
      this.send(s, "MAIL_LIST", { mailList });
    },

    SEND_MAIL(s, { targetPlayerID, chainID, message, subject }) {
      const target = this.store.player(targetPlayerID);
      if (!target) {
        this.send(s, "SEND_MAIL_RESPONSE", { responseCode: 1, targetplayerID: targetPlayerID });
        return;
      }
      const mailId = this.store.addMail({
        receiverId: targetPlayerID,
        senderId: s.playerId,
        senderName: this.profile(s.playerId).profileName ?? "",
        chainId: chainID,
        message,
        subject,
        created: formatDate(),
      });
      this.send(s, "SEND_MAIL_RESPONSE", {
        responseCode: 0,
        targetplayerID: targetPlayerID,
        targetPlayerName: target.profile.profileName ?? "",
      });
      this.sendToPlayer(targetPlayerID, "NEW_MAIL", { mail: this.mailEntity(this.store.mailById(mailId)) });
    },

    CHECKED_MAIL(s, { mailID }) {
      this.store.markMailRead(s.playerId, mailID);
    },
    DELETE_MAIL(s, { mailID }) {
      this.store.deleteMail(s.playerId, mailID);
    },

    // Bulletin boards

    GET_ALL_BULLETIN_MESSAGE(s, { locationID }) {
      const messageList = this.store.bulletins(locationID).map((row) => ({
        bulletinMessageID: row.message_id,
        playerID: row.player_id,
        playerName: row.player_name,
        message: row.message,
        timestamp: row.timestamp,
        locationID: row.location_id,
      }));
      this.send(s, "ALL_BULLETIN_MESSAGE_LIST", { postRemainTime: 0, messageList });
    },

    ADD_BULLETIN_MESSAGE(s, { message }) {
      this.store.addBulletin({
        locationId: message.locationID,
        playerId: s.playerId,
        playerName: this.profile(s.playerId).profileName ?? "",
        message: message.message,
        timestamp: formatDate(),
      });
    },

    // Ranks and ratings

    CREW_RANK(s) {
      const totals = new Map();
      for (const { id, reputation } of this.store.topPlayers(Infinity)) {
        const district = this.profile(id).districtID ?? 0;
        totals.set(district, (totals.get(district) ?? 0) + reputation);
      }
      const rankList = DISTRICTS.map((rankName, rankID) => ({
        rankID,
        rankName,
        reputation: totals.get(rankID) ?? 0,
      }))
        .sort((a, b) => b.reputation - a.reputation)
        .map((r, rankIndex) => ({ ...r, rankIndex }));
      this.send(s, "CREW_RANK_RESPONSE", { responseCode: 0, rankList });
    },

    PLAYER_RANK(s, { targetPlayerID, startWorldRank, endWorldRank }) {
      const all = this.store.topPlayers(Infinity);
      const entity = ({ id, reputation }, rankIndex) => ({
        rankID: id,
        rankName: this.profile(id).profileName ?? "",
        reputation,
        rankIndex,
      });
      const rankList = all.slice(startWorldRank, endWorldRank + 1).map((p, i) => entity(p, startWorldRank + i));
      const index = all.findIndex((p) => p.id === targetPlayerID);
      const playerRank =
        index >= 0 ? entity(all[index], index) : { rankID: targetPlayerID, rankName: "", reputation: 0, rankIndex: all.length };
      this.send(s, "PLAYER_RANK_RESPONSE", { responseCode: 0, playerRank, rankList });
    },

    GET_PLAYER_RATING_LIST(s, { targetPlayerID, playerRatingList }) {
      const counts = new Map(this.store.ratingCounts(targetPlayerID).map((r) => [r.rating, r.n]));
      this.send(s, "PLAYER_RATING_LIST", {
        playerRatingCountList: playerRatingList.map((rating) => ({ rating, numOfRating: counts.get(rating) ?? 0 })),
      });
    },

    ADD_PLAYER_RATING(s, { targetPlayerID, rating }) {
      const added = targetPlayerID !== s.playerId && this.store.addRating(s.playerId, targetPlayerID, rating);
      this.send(s, "ADD_PLAYER_RATING_RESPONSE", { responseCode: added ? 0 : 1 });
    },

    SEARCH_PLAYER(s, { name }) {
      const playerProfileList = this.store.searchPlayers(name).map((id) => this.profile(id));
      this.send(s, "SEARCH_PLAYER_RESULT_LIST", { playerProfileList });
    },

    SEARCH_FB_FRIEND(s) {
      this.send(s, "SEARCH_FBFRIEND_RESULT_LIST", { fbFriendProfileList: [] });
    },

    // Other startup data

    GET_ALL_NEWS(s) {
      const newsList = this.store.news().map((row) => ({
        newsID: row.news_id,
        created: row.created,
        subject: row.subject,
        content: row.content,
        link: row.link,
      }));
      this.send(s, "NEWS_LIST", { newsList });
    },

    GET_PROFANITY_WORDS_ENTITY(s) {
      // The client also filters with the list in its own lp.sqlite.
      this.send(s, "PROFANITY_WORD_ENTITIES", { wordEntityList: [] });
    },
  };

  requestEntity(row) {
    return {
      requestID: row.request_id,
      fromPlayerID: row.from_id,
      targetPlayerID: row.target_id,
      type: row.type,
      fromPlayerProfile: this.profile(row.from_id),
      isInstance: false,
    };
  }

  mailEntity(row) {
    return {
      mailID: row.mail_id,
      receiverPlayerID: row.receiver_id,
      senderPlayerID: row.sender_id,
      chainID: row.chain_id,
      message: row.message,
      subject: row.subject,
      mediaID: -1,
      created: row.created,
      updated: row.updated,
      senderName: row.sender_name,
      hadRead: !!row.read,
    };
  }
}

/** Messages accepted before a player is logged in. */
const PRE_LOGIN = new Set([
  CHANNEL_SETUP,
  "GUEST_LOGIN",
  "CREATE_ACCOUNT",
  "LOGIN",
  "FORGOT_PASSWORD",
  "CREATE_PROFILE",
  "ACCOUNT_UPDATE",
  "GAME_ID_UPDATE",
]);
